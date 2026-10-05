#!/usr/bin/env python3
"""Stage one native CI binary with a checksum and reproducible build metadata."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tomllib
from pathlib import Path


def output(command: list[str]) -> str:
    return subprocess.check_output(command, text=True).strip()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--os", required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(f"built binary not found: {args.binary}")
    package = tomllib.loads(Path("Cargo.toml").read_text(encoding="utf-8"))["package"]
    version = package["version"]
    working_tree_clean = not output(["git", "status", "--porcelain"])
    output_dir = args.output
    output_dir.mkdir(parents=True, exist_ok=True)
    filename = f"newgit-{version}-{args.target}" + (".exe" if args.binary.suffix.lower() == ".exe" else "")
    staged_binary = output_dir / filename
    shutil.copy2(args.binary, staged_binary)
    if args.binary.suffix.lower() != ".exe" and (staged_binary.stat().st_mode & 0o111) == 0:
        raise SystemExit(f"staged POSIX binary is not executable: {staged_binary}")
    digest = hashlib.sha256(staged_binary.read_bytes()).hexdigest()

    metadata = {
        "binary": filename,
        "package": package["name"],
        "version": version,
        "target": args.target,
        "operating_system": args.os,
        "architecture": args.architecture,
        "source_commit": output(["git", "rev-parse", "HEAD"]),
        "working_tree_clean": working_tree_clean,
        "rustc": output(["rustc", "--version"]),
        "git": output(["git", "--version"]),
        "sha256": digest,
    }
    (output_dir / "SHA256SUMS.txt").write_text(
        f"{digest}  {filename}\n", encoding="utf-8"
    )
    (output_dir / "build-metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps(metadata, sort_keys=True))


if __name__ == "__main__":
    main()
