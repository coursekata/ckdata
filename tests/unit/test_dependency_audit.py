"""Check complete reporting of every locked dependency."""

import json

import pytest

from scripts import audit_dependencies as audit


@pytest.fixture
def packages():
    return [{"ecosystem": "crates.io", "name": "synthetic", "version": "1.0"}]


def test_partial_osv_page_fails_instead_of_reporting_no_findings(packages):
    with pytest.raises(ValueError, match="partial page"):
        audit.osv_advisories(
            packages,
            lambda *_: {"results": [{"next_page_token": "next", "vulns": []}]},
        )


def test_missing_osv_result_fails(packages):
    with pytest.raises(ValueError, match="one result per"):
        audit.osv_advisories(packages, lambda *_: {"results": []})


def test_informational_record_is_retained_in_full(packages):
    record = {
        "id": "RUSTSEC-SYNTHETIC",
        "database_specific": {"informational": "unmaintained"},
    }

    def fetch(url, _data=None):
        if url.endswith("querybatch"):
            return {"results": [{"vulns": [{"id": record["id"]}]}]}
        return record

    result = audit.osv_advisories(packages, fetch)
    assert result["advisories"][record["id"]] == record
    assert result["matches"][0]["package"] == packages[0]


def test_node_inventory_keeps_nested_versions_and_tree_errors():
    tree = {
        "name": "renovate",
        "version": "1.0",
        "problems": ["missing: synthetic"],
        "dependencies": {
            "example": {
                "version": "1.0",
                "dependencies": {"example": {"version": "2.0"}},
            }
        },
    }
    result = audit.npm_advisories(tree, lambda _url, data: {"submitted": data})
    assert result["installed_versions"]["example"] == ["1.0", "2.0"]
    assert result["tree_problems"] == ["missing: synthetic"]


def test_export_and_scanner_verification_cover_all_python_lock_branches(tmp_path):
    (tmp_path / "r/src/rust").mkdir(parents=True)
    for path in (tmp_path / "Cargo.lock", tmp_path / "r/src/rust/Cargo.lock"):
        path.write_text(
            '[[package]]\nname="synthetic"\nversion="1.0"\nsource="registry+https://github.com/rust-lang/crates.io-index"\n'
        )
    (tmp_path / "uv.lock").write_text("""[[package]]
name="ckdata-development"
version="0.0.0"
source={virtual="."}
[[package]]
name="example"
version="1.0"
source={registry="https://pypi.org/simple"}
[[package]]
name="example"
version="2.0"
source={registry="https://pypi.org/simple"}
""")
    output = tmp_path / "inventory"
    audit.export_inventory(tmp_path, output)
    assert (output / "requirements.txt").read_text() == "example==1.0\nexample==2.0\n"
    report = tmp_path / "report.json"
    report.write_text(
        json.dumps(
            {
                "Results": [
                    {"Type": "pip", "Packages": [{"Name": "example", "Version": "1.0"}]}
                ]
            }
        )
    )
    with pytest.raises(ValueError, match="missing"):
        audit.verify_inventory(tmp_path, report)
    report.write_text(
        json.dumps(
            {
                "Results": [
                    {
                        "Type": "pip",
                        "Packages": [
                            {"Name": "example", "Version": "1.0"},
                            {"Name": "example", "Version": "2.0"},
                        ],
                    }
                ]
            }
        )
    )
    audit.verify_inventory(tmp_path, report)
