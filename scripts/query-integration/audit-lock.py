"""Make every locked script dependency visible to the vulnerability scanner."""

import argparse
import json
import re
import shutil
import tomllib
from pathlib import Path


def locked_packages():
    lock = Path(__file__).with_name("check.py.lock")
    with lock.open("rb") as source:
        packages = tomllib.load(source)["package"]
    result = set()
    for package in packages:
        name, version = package["name"], package["version"]
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", name) or not re.fullmatch(
            r"[A-Za-z0-9_.+!-]+", version
        ):
            raise ValueError("unexpected locked package identifier")
        result.add((name.lower().replace("_", "-"), version))
    if not result:
        raise ValueError("the script lock contains no packages")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--verify", type=Path)
    args = parser.parse_args()
    packages = locked_packages()
    if args.output:
        args.output.mkdir(parents=True, exist_ok=True)
        # Requirements have no dev-group filtering. Include every version/marker branch.
        (args.output / "requirements.txt").write_text(
            "".join(f"{name}=={version}\n" for name, version in sorted(packages))
        )
        root = Path(__file__).resolve().parents[2]
        for label, lock in (
            ("rust", root / "Cargo.lock"),
            ("r-rust", root / "r/src/rust/Cargo.lock"),
        ):
            directory = args.output / label
            directory.mkdir(exist_ok=True)
            shutil.copyfile(lock, directory / "Cargo.lock")
    if args.verify:
        report = json.loads(args.verify.read_text())
        reported = {
            (package["Name"].lower().replace("_", "-"), package["Version"])
            for result in report.get("Results", [])
            if result.get("Type") == "pip"
            for package in result.get("Packages", [])
        }
        missing = packages - reported
        if missing:
            raise RuntimeError(
                f"script-lock scanner coverage missing: {sorted(missing)}"
            )
        print(
            f"PASS vulnerability inventory covers all {len(packages)} script-lock packages"
        )
    if not args.output and not args.verify:
        parser.error("choose --output or --verify")


if __name__ == "__main__":
    main()
