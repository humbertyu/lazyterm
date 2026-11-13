#!/usr/bin/env python3
"""Fail releases on advisories reachable in either selected macOS build graph."""
import argparse
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database")
    args = parser.parse_args()
    selected = set()
    for target in TARGETS:
        output = subprocess.check_output(
            ["cargo", "tree", "--locked", "--target", target, "--prefix", "none", "--format", "{p}"],
            cwd=ROOT, text=True)
        selected.update(re.findall(r"(?m)^(\S+) v(\S+)", output))
    command = ["cargo", "audit", "--json"]
    if args.database:
        command.extend(["--db", args.database, "--no-fetch"])
    scan = subprocess.run(command, cwd=ROOT, text=True, capture_output=True)
    if scan.returncode not in (0, 1) or not scan.stdout.strip():
        raise SystemExit("Advisory scan failed: " + scan.stderr)
    report = json.loads(scan.stdout)
    findings = report["vulnerabilities"]["list"] or []
    for entries in report["warnings"].values():
        findings.extend(entries)
    blocked = False
    for finding in findings:
        package = finding["package"]
        advisory = finding["advisory"]
        active = (package["name"], package["version"]) in selected
        informational = advisory.get("informational")
        fatal = not informational or informational == "unsound"
        print("{} {} {} {}: {}".format(
            "BLOCK" if active and fatal else "WARN" if active else "DORMANT",
            package["name"], package["version"], advisory["id"], advisory["title"]))
        blocked |= active and fatal
    print("Checked both default macOS build graphs; dormant optional dependencies remain reported.")
    raise SystemExit(1 if blocked else 0)


if __name__ == "__main__":
    main()
