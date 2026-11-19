#!/usr/bin/env python3
"""Package the portable Skill, optionally pinned to a local development binary."""

import argparse
from pathlib import Path
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Destination ZIP")
    parser.add_argument("--binary", type=Path, help="Pin this package to an existing local executable")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    binary = args.binary.expanduser().resolve() if args.binary else None
    if binary:
        import os
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error("--binary must point to an existing executable; run cargo build first")
        if any(character in str(binary) for character in "\n\r`"):
            parser.error("--binary path cannot contain newlines or backticks")
    source = root / "skills/lazyterm/SKILL.md"
    output = args.output.expanduser().resolve()
    if output == source or output == binary:
        parser.error("--output must not replace the Skill source or executable")
    output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.write(source, "lazyterm/SKILL.md")
        if binary:
            archive.writestr(
                "lazyterm/references/local-runtime.md",
                "# Local Development Runtime\n\n"
                "Use this absolute executable path for every LazyTerm command:\n\n"
                f"```text\n{binary}\n```\n\n"
                "This is a development build on the user's machine. Do not use a "
                "different lazyterm from PATH. If unavailable, report the path and "
                "ask the user to build the checkout. Do not install or compile "
                "automatically. A subsequent cargo build updates this executable "
                "without reinstalling this Skill; a moved checkout requires a new package.\n",
            )
    print(output)


if __name__ == "__main__":
    main()
