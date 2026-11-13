import hashlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("release", Path(__file__).resolve().parents[1] / "scripts/release.py")
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)
VERSION = "0.6.0-beta.1"


def make_archive(root, target, extra=None, dirty=False):
    name = release.archive_name(VERSION, target)
    prefix = name[:-7] + "/"
    binary = b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x100000C if target.startswith("aarch64") else 0x1000007)
    info = {"version": VERSION, "target": target, "source_dirty": dirty, "commit": "same-source", "binary_sha256": hashlib.sha256(binary).hexdigest()}
    with tarfile.open(root / name, "w:gz") as archive:
        for document in release.DOCUMENTS + ("lazyterm", "build-info.json", "docs"):
            entry = tarfile.TarInfo(prefix + document)
            data = binary if document == "lazyterm" else json.dumps(info).encode() if document == "build-info.json" else b"fixture"
            if document == "docs":
                entry.type = tarfile.DIRTYPE
                archive.addfile(entry)
            else:
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        if extra:
            archive.addfile(tarfile.TarInfo(extra))
    return root / name


def universal_bytes(slices):
    offset = 48
    entries, bodies = [], []
    for binary in slices:
        cpu = struct.unpack("<I", binary[4:8])[0]
        entries.append(struct.pack(">IIIII", cpu, 0, offset, len(binary), 0))
        bodies.append(binary)
        offset += len(binary)
    return b"\xca\xfe\xba\xbe" + struct.pack(">I", len(slices)) + b"".join(entries + bodies)


class ReleaseTests(unittest.TestCase):
    def test_assembly_checks_assets_and_generates_real_formula_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for target in release.TARGETS:
                make_archive(root, target)
            def run(command, **kwargs):
                if command[0] == "lipo":
                    Path(command[-1]).write_bytes(universal_bytes([Path(path).read_bytes() for path in command[2:4]]))

            def capture(*command):
                return "123456" if command[0] == "git" else "lazyterm " + VERSION

            with patch.object(release, "version", return_value=VERSION), patch.object(
                    release.subprocess, "run", side_effect=run) as runner, patch.object(
                    release, "capture", side_effect=capture):
                release.assemble(SimpleNamespace(output=root, allow_dirty=False))
            runner.assert_any_call(["codesign", "--verify", "--strict", "--all-architectures",
                                    runner.call_args_list[0].args[0][-1]], check=True)
            formula = (root / "lazyterm.rb").read_text()
            path = root / release.archive_name(VERSION, release.UNIVERSAL)
            self.assertIn(release.digest(path), formula)
            self.assertIn(path.name, formula)
            self.assertNotIn("on_intel", formula)
            info = release.inspect_archive(path, VERSION, release.UNIVERSAL)
            self.assertEqual(info["architectures"], list(release.TARGETS))
            for line in (root / "SHA256SUMS").read_text().splitlines():
                checksum, filename = line.split("  ")
                self.assertEqual(checksum, release.digest(root / filename))
            self.assertNotIn("PLACEHOLDER", formula)

    def test_universal_rejects_duplicate_mismatched_and_overlapping_slices(self):
        arm = b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x100000C)
        intel = b"\xcf\xfa\xed\xfe" + struct.pack("<I", 0x1000007)
        valid = universal_bytes([arm, intel])
        self.assertEqual(release.binary_cpus(valid), {0x100000C, 0x1000007})
        with self.assertRaises(ValueError):
            release.binary_cpus(universal_bytes([arm, arm]))
        invalid = bytearray(valid)
        struct.pack_into(">I", invalid, 36, 48)
        with self.assertRaises(ValueError):
            release.binary_cpus(invalid)
        invalid = bytearray(valid)
        invalid[52:56] = intel[4:8]
        with self.assertRaises(ValueError):
            release.binary_cpus(invalid)

    def test_unexpected_paths_and_dirty_sources_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = make_archive(root, release.TARGETS[0], extra="../../.ssh/id_ed25519")
            with self.assertRaises(ValueError):
                release.inspect_archive(path, VERSION, release.TARGETS[0])
            for target in release.TARGETS:
                make_archive(root, target, dirty=True)
            with patch.object(release, "version", return_value=VERSION), self.assertRaises(ValueError):
                release.assemble(SimpleNamespace(output=root, allow_dirty=False))
            self.assertFalse((root / "SHA256SUMS").exists())

    def test_wrong_architecture_and_unsafe_version_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = make_archive(root, release.TARGETS[0])
            with self.assertRaises(ValueError):
                release.inspect_archive(path, VERSION, release.TARGETS[1])
            with self.assertRaises(ValueError):
                release.archive_name("../../escape", release.TARGETS[0])


if __name__ == "__main__":
    unittest.main()
