"""Verify that an R source distribution is self-contained."""

import argparse
import tarfile
from pathlib import Path


def check_tarball(path):
    with tarfile.open(path, "r:gz") as archive:
        members = archive.getmembers()
    roots = {member.name.split("/", 1)[0] for member in members}
    if len(roots) != 1:
        raise ValueError("R tarball must contain one package root")
    core = next(iter(roots)) + "/src/rust/core/Cargo.toml"
    if not any(member.name == core and member.isfile() for member in members):
        raise ValueError("R tarball does not carry the core as real files")
    if any(member.issym() or member.islnk() for member in members):
        raise ValueError("R tarball must not contain links")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tarball", type=Path)
    args = parser.parse_args()
    check_tarball(args.tarball)
    print(f"{args.tarball} carries the core and no links")


if __name__ == "__main__":
    main()
