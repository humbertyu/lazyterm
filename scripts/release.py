#!/usr/bin/env python3
"""Build private-data-free macOS archives and generate verified release metadata."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import struct
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
UNIVERSAL = "universal-apple-darwin"
DOCUMENTS = ("README.md", "LICENSE", "docs/INSTALL.md", "docs/SECURITY-REVIEW.md")


def capture(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def version():
    metadata = json.loads(capture("cargo", "metadata", "--locked", "--no-deps", "--format-version=1", "--offline"))
    return next(p["version"] for p in metadata["packages"] if p["name"] == "lazyterm")


def archive_name(release_version, target):
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[a-zA-Z0-9.-]+)?", release_version):
        raise ValueError("Invalid release version")
    if target not in (*TARGETS, UNIVERSAL):
        raise ValueError("Unsupported release target")
    return "lazyterm-v{}-{}.tar.gz".format(release_version, target)


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def build(args):
    release_version = version()
    dirty = bool(capture("git", "status", "--porcelain"))
    if dirty and not args.allow_dirty:
        raise ValueError("Commit changes before a release build, or use --allow-dirty for local testing")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    environment = dict(os.environ, MACOSX_DEPLOYMENT_TARGET="15.0")
    subprocess.run(["cargo", "build", "--release", "--locked", "--target", args.target],
                   cwd=ROOT, env=environment, check=True)
    binary = ROOT / "target" / args.target / "release" / "lazyterm"
    expected_arch = "arm64" if args.target.startswith("aarch64") else "x86_64"
    if capture("lipo", "-archs", str(binary)) != expected_arch:
        raise ValueError("Release binary has an unexpected architecture")
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
    dylibs = [line.strip().split(" (", 1)[0] for line in capture("otool", "-L", str(binary)).splitlines()[1:]]
    if any(not item.startswith(("/usr/lib/", "/System/Library/")) for item in dylibs):
        raise ValueError("Release binary links a non-system library: " + repr(dylibs))
    if not re.search(r"\bminos 15\.0\b", capture("otool", "-l", str(binary))):
        raise ValueError("Release binary does not declare the macOS 15 deployment baseline")
    if capture(str(binary), "--version") != "lazyterm " + release_version:
        raise ValueError("Release version smoke test failed (cross-builds require Rosetta)")
    subprocess.run([str(binary), "--diagnostics"], check=True)
    commit = capture("git", "rev-parse", "HEAD")
    stamp = int(capture("git", "show", "-s", "--format=%ct", "HEAD"))
    info = {
        "version": release_version, "target": args.target, "commit": commit,
        "source_dirty": dirty, "macos_minimum": "15.0", "signing": "ad-hoc",
        "rustc": capture("rustc", "--version"), "sdk": capture("xcrun", "--show-sdk-version"),
        "binary_sha256": digest(binary), "system_libraries": dylibs,
    }
    package(binary, info, output, stamp)


def package(binary, info, output, stamp):
    name = archive_name(info["version"], info["target"])
    with tempfile.TemporaryDirectory(prefix="lazyterm-package-") as temporary:
        stage = Path(temporary) / name[:-7]
        stage.mkdir()
        for relative in DOCUMENTS:
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes((ROOT / relative).read_bytes())
        (stage / "lazyterm").write_bytes(binary.read_bytes())
        (stage / "build-info.json").write_text(json.dumps(info, indent=2) + "\n")
        partial = output / (name + ".tmp")
        try:
            with partial.open("wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=stamp, filename="") as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as archive:
                    for path in sorted(stage.rglob("*")):
                        entry = archive.gettarinfo(str(path), str(path.relative_to(stage.parent)))
                        entry.uid = entry.gid = 0
                        entry.uname = entry.gname = ""
                        entry.mtime = stamp
                        entry.mode = 0o755 if path.is_dir() or path.name == "lazyterm" else 0o644
                        if path.is_file():
                            with path.open("rb") as source:
                                archive.addfile(entry, source)
                        else:
                            archive.addfile(entry)
            partial.replace(output / name)
        finally:
            partial.unlink(missing_ok=True)
    print(output / name)


def inspect_archive(path, release_version, target):
    prefix = archive_name(release_version, target)[:-7] + "/"
    expected = set(DOCUMENTS) | {"lazyterm", "build-info.json", "docs"}
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
        names = set()
        for entry in members:
            if not entry.name.startswith(prefix) or entry.name[len(prefix):] not in expected:
                raise ValueError("Unexpected archive path: " + entry.name)
            if not (entry.isfile() or entry.isdir()) or entry.issym() or entry.islnk():
                raise ValueError("Release archives must not contain links or special files")
            if entry.name in names:
                raise ValueError("Duplicate archive member")
            names.add(entry.name)
        if names != {prefix + name for name in expected}:
            raise ValueError("Incomplete release archive")
        info = json.load(archive.extractfile(prefix + "build-info.json"))
        if info["version"] != release_version or info["target"] != target:
            raise ValueError("Archive metadata does not match the release")
        binary = archive.extractfile(prefix + "lazyterm").read()
        expected_cpus = {0x100000C, 0x1000007} if target == UNIVERSAL else {
            0x100000C if target.startswith("aarch64") else 0x1000007}
        if binary_cpus(binary) != expected_cpus:
            raise ValueError("Archive executable architecture does not match its target")
        if hashlib.sha256(binary).hexdigest() != info["binary_sha256"]:
            raise ValueError("Archive executable checksum does not match its build record")
        return info


def binary_cpus(binary):
    if len(binary) >= 8 and binary[:4] == b"\xcf\xfa\xed\xfe":
        return {struct.unpack("<I", binary[4:8])[0]}
    if len(binary) < 8 or binary[:4] != b"\xca\xfe\xba\xbe":
        raise ValueError("Expected a Mach-O executable")
    count = struct.unpack(">I", binary[4:8])[0]
    if count != 2 or len(binary) < 8 + count * 20:
        raise ValueError("Expected exactly two universal architectures")
    cpus, ranges = set(), []
    for index in range(count):
        cpu, _, offset, size, alignment = struct.unpack_from(">IIIII", binary, 8 + index * 20)
        if (cpu in cpus or size < 8 or offset < 8 + count * 20
                or offset + size > len(binary) or alignment > 30
                or offset % (1 << alignment) or any(offset < end and start < offset + size for start, end in ranges)):
            raise ValueError("Invalid universal architecture table")
        if binary_cpus(binary[offset:offset + size]) != {cpu}:
            raise ValueError("Universal slice does not match its architecture table")
        cpus.add(cpu)
        ranges.append((offset, offset + size))
    return cpus


def formula(release_version, base_url, hashes):
    return '''class Lazyterm < Formula
  desc "Keyboard-driven SSH and project workspace"
  homepage "https://github.com/humbertyu/lazyterm"
  version "{version}"
  license "MIT"

  url "{base}/{universal}"
  sha256 "{universal_hash}"

  depends_on :macos
  depends_on macos: :sequoia

  def install
    bin.install "lazyterm"
    doc.install "docs/INSTALL.md", "docs/SECURITY-REVIEW.md", "build-info.json"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/lazyterm --version")
    assert_match "Architecture:", shell_output("#{{bin}}/lazyterm --diagnostics")
  end
end
'''.format(version=release_version, base=base_url.rstrip("/"),
           universal=archive_name(release_version, UNIVERSAL), universal_hash=hashes[UNIVERSAL])


def assemble(args):
    release_version = version()
    output = args.output.resolve()
    hashes, infos, paths = {}, [], []
    for target in TARGETS:
        path = output / archive_name(release_version, target)
        infos.append(inspect_archive(path, release_version, target))
        hashes[target] = digest(path)
        paths.append(path)
    if len({info["commit"] for info in infos}) != 1:
        raise ValueError("Architectures were built from different commits")
    if any(info["source_dirty"] for info in infos) and not args.allow_dirty:
        raise ValueError("Dirty-source archives are for local testing only")
    with tempfile.TemporaryDirectory(prefix="lazyterm-universal-") as temporary:
        stage = Path(temporary)
        slices = []
        for target, path in zip(TARGETS, paths):
            binary = stage / target
            with tarfile.open(path, "r:gz") as archive:
                binary.write_bytes(archive.extractfile(archive_name(release_version, target)[:-7] + "/lazyterm").read())
            slices.append(str(binary))
        binary = stage / "lazyterm"
        subprocess.run(["lipo", "-create", *slices, "-output", str(binary)], check=True)
        binary.chmod(0o755)
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(binary)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", "--all-architectures", str(binary)], check=True)
        if binary_cpus(binary.read_bytes()) != {0x100000C, 0x1000007}:
            raise ValueError("Universal binary must include arm64 and x86_64")
        if capture(str(binary), "--version") != "lazyterm " + release_version:
            raise ValueError("Universal binary version smoke test failed")
        subprocess.run([str(binary), "--diagnostics"], check=True)
        info = {
            "version": release_version, "target": UNIVERSAL, "commit": infos[0]["commit"],
            "source_dirty": any(item["source_dirty"] for item in infos),
            "macos_minimum": "15.0", "signing": "ad-hoc",
            "architectures": list(TARGETS), "builds": infos, "binary_sha256": digest(binary),
        }
        package(binary, info, output, int(capture("git", "show", "-s", "--format=%ct", infos[0]["commit"])))
    path = output / archive_name(release_version, UNIVERSAL)
    inspect_archive(path, release_version, UNIVERSAL)
    hashes[UNIVERSAL] = digest(path)
    paths = [path]
    base = "https://github.com/humbertyu/lazyterm/releases/download/v" + release_version
    generated = output / "lazyterm.rb"
    generated.write_text(formula(release_version, base, hashes))
    paths.append(generated)
    (output / "SHA256SUMS").write_text("".join("{}  {}\n".format(digest(path), path.name) for path in paths))
    print(output / "SHA256SUMS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    commands.add_parser("version")
    for action in ("build", "assemble", "universal"):
        command = commands.add_parser(action)
        command.add_argument("--output", type=Path, default=ROOT / "dist")
        command.add_argument("--allow-dirty", action="store_true")
        if action == "build":
            command.add_argument("--target", required=True, choices=TARGETS)
    args = parser.parse_args()
    if args.action == "version":
        print(version())
    elif args.action == "build":
        build(args)
    elif args.action == "universal":
        for target in TARGETS:
            args.target = target
            build(args)
        assemble(args)
    else:
        assemble(args)


if __name__ == "__main__":
    main()
