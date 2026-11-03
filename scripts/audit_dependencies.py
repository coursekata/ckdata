"""Report every OSV advisory in the two Cargo locks and shared Python development lock.

Optionally query npm's bulk endpoint for an existing Renovate hook tree captured
with npm ls --all --json. This command never installs or modifies dependencies.
Findings, including informational and unfixed records, remain in the report.
"""

import argparse
import json
import re
import shutil
import sys
import tomllib
from datetime import UTC, datetime
from pathlib import Path
from urllib.request import Request, urlopen

OSV = "https://api.osv.dev/v1/"
NPM = "https://registry.npmjs.org/-/npm/v1/security/advisories/bulk"


def request_json(url, data=None):
    request = Request(url, headers={"User-Agent": "ckdata-dependency-inventory"})
    if data is not None:
        request.data = json.dumps(data).encode()
        request.add_header("Content-Type", "application/json")
    with urlopen(request, timeout=60) as response:
        return json.load(response)


def locked_packages(root):
    packages = {}
    locks = (
        ("Cargo.lock", "crates.io"),
        ("r/src/rust/Cargo.lock", "crates.io"),
        ("uv.lock", "PyPI"),
    )
    for relative, ecosystem in locks:
        with (root / relative).open("rb") as source:
            records = tomllib.load(source)["package"]
        count = 0
        for package in records:
            if ecosystem == "crates.io":
                if (
                    package.get("source")
                    != "registry+https://github.com/rust-lang/crates.io-index"
                ):
                    continue
            elif package.get("source", {}).get("registry") != "https://pypi.org/simple":
                continue
            name, version = package["name"], package["version"]
            if not re.fullmatch(r"[A-Za-z0-9_.-]+", name) or not re.fullmatch(
                r"[A-Za-z0-9_.+!-]+", version
            ):
                raise ValueError("unexpected public dependency identifier")
            packages.setdefault((ecosystem, name, version), []).append(relative)
            count += 1
        if not count:
            raise ValueError(f"no registry packages found in {relative}")
    return [
        {"ecosystem": key[0], "name": key[1], "version": key[2], "locks": locks}
        for key, locks in sorted(packages.items())
    ]


def osv_advisories(packages, fetch=request_json):
    queries = [
        {
            "package": {"ecosystem": p["ecosystem"], "name": p["name"]},
            "version": p["version"],
        }
        for p in packages
    ]
    matches = []
    records = {}
    for start in range(0, len(queries), 1000):
        batch = queries[start : start + 1000]
        response = fetch(OSV + "querybatch", {"queries": batch})
        results = response["results"]
        if len(results) != len(batch):
            raise ValueError("OSV did not return one result per queried package")
        for package, result in zip(
            packages[start : start + 1000], results, strict=True
        ):
            if result.get("next_page_token"):
                raise ValueError(
                    "OSV returned a partial page; complete advisory inventory unavailable"
                )
            for vulnerability in result.get("vulns", []):
                advisory_id = vulnerability["id"]
                if not re.fullmatch(r"[A-Za-z0-9_-]+", advisory_id):
                    raise ValueError("unexpected OSV advisory identifier")
                matches.append({"package": package, "advisory_id": advisory_id})
                if advisory_id not in records:
                    records[advisory_id] = fetch(OSV + "vulns/" + advisory_id)
    return {"queries": queries, "matches": matches, "advisories": records}


def npm_advisories(tree, fetch=request_json):
    if tree.get("name") != "renovate" or not tree.get("version"):
        raise ValueError("npm input must describe the existing Renovate hook package")
    versions = {}
    problems = set(tree.get("problems", []))
    pending = [(tree["name"], tree)]
    while pending:
        name, package = pending.pop()
        version = package.get("version")
        if version:
            versions.setdefault(name, set()).add(version)
        problems.update(package.get("problems", []))
        pending.extend(package.get("dependencies", {}).items())
    payload = {name: sorted(values) for name, values in sorted(versions.items())}
    if not payload:
        raise ValueError("npm input contains no installed package versions")
    return {
        "tool_version": tree["version"],
        "installed_versions": payload,
        "tree_problems": sorted(problems),
        "tree_error": tree.get("error"),
        "advisories": fetch(NPM, payload),
        "limits": "Existing installed versions only; missing dependencies and npm metavulnerability totals are not resolved. Advisory ranges remain in full.",
    }


def summary(report):
    lines = ["### Complete dependency advisory inventory"]
    for match in report["osv"]["matches"]:
        package = match["package"]
        advisory = report["osv"]["advisories"][match["advisory_id"]]
        label = next(
            (
                affected.get("database_specific", {}).get("informational")
                for affected in advisory.get("affected", [])
                if affected.get("database_specific", {}).get("informational")
            ),
            "advisory",
        )
        lines.append(
            f"- {package['name']} {package['version']}: {match['advisory_id']} ({label})"
        )
    npm = report.get("npm")
    if npm is None:
        lines.append(
            "- Node tooling was not audited in this run; no cleanliness claim is made."
        )
    else:
        for name, advisories in npm["advisories"].items():
            for advisory in advisories:
                lines.append(
                    f"- npm {name}: {advisory['severity']}, {advisory['url']}, affected range {advisory['vulnerable_versions']}"
                )
        if npm["tree_problems"] or npm["tree_error"]:
            lines.append(
                "- npm reported an incomplete tree; installed version records were audited and the tree problems are retained in the JSON."
            )
    lines.append(
        "All severities, informational records and unfixed advisories are retained in the JSON. Findings require maintainer review under the dependency policy."
    )
    return "\n".join(lines) + "\n"


def python_packages(root):
    return {
        (p["name"].lower().replace("_", "-"), p["version"])
        for p in locked_packages(root)
        if p["ecosystem"] == "PyPI"
    }


def export_inventory(root, output):
    output.mkdir(parents=True, exist_ok=True)
    # Scanner input only: preserve every locked platform/group branch, never install it.
    (output / "requirements.txt").write_text(
        "".join(
            f"{name}=={version}\n" for name, version in sorted(python_packages(root))
        )
    )
    for label, relative in (
        ("rust", "Cargo.lock"),
        ("r_rust", "r/src/rust/Cargo.lock"),
    ):
        directory = output / label
        directory.mkdir(exist_ok=True)
        shutil.copyfile(root / relative, directory / "Cargo.lock")


def verify_inventory(root, path):
    report = json.loads(path.read_text())
    reported = {
        (p["Name"].lower().replace("_", "-"), p["Version"])
        for result in report.get("Results", [])
        if result.get("Type") == "pip"
        for p in result.get("Packages", [])
    }
    packages = python_packages(root)
    missing = packages - reported
    if missing:
        raise ValueError(f"Python scanner coverage missing: {sorted(missing)}")
    print(
        f"PASS vulnerability inventory covers all {len(packages)} Python lock records"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parents[1]
    )
    commands = parser.add_subparsers(dest="command", required=True)
    export = commands.add_parser(
        "export", help="export all locked versions for scanning"
    )
    export.add_argument("--output", type=Path, required=True)
    verify = commands.add_parser(
        "verify", help="verify the scanner covered the Python lock"
    )
    verify.add_argument("report", type=Path)
    scan = commands.add_parser(
        "scan", help="query every advisory severity, including informational"
    )
    scan.add_argument("--npm-tree", type=Path)
    scan.add_argument("--output", type=Path, required=True)
    scan.add_argument("--summary", type=Path)
    args = parser.parse_args()
    if args.command == "export":
        export_inventory(args.root, args.output)
        return
    if args.command == "verify":
        verify_inventory(args.root, args.report)
        return
    packages = locked_packages(args.root)
    report = {
        "checked_at_utc": datetime.now(UTC).isoformat(),
        "packages": packages,
        "osv": osv_advisories(packages),
    }
    if args.npm_tree:
        report["npm"] = npm_advisories(json.loads(args.npm_tree.read_text()))
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    rendered = summary(report)
    if args.summary:
        with args.summary.open("a") as destination:
            destination.write(rendered)
    else:
        print(rendered, end="")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"dependency advisory inventory failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
