"""Offline regression checks for audit completeness and the live SDK gate."""

import builtins
import importlib.util
import unittest
from pathlib import Path
from unittest.mock import patch


def load(name, filename):
    spec = importlib.util.spec_from_file_location(
        name, Path(__file__).with_name(filename)
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.audit = load("audit_advisories", "audit-advisories.py")
        self.packages = [
            {"ecosystem": "crates.io", "name": "synthetic", "version": "1.0"}
        ]

    def test_partial_osv_page_fails_instead_of_reporting_no_findings(self):
        with self.assertRaisesRegex(ValueError, "partial page"):
            self.audit.osv_advisories(
                self.packages,
                lambda *_: {"results": [{"next_page_token": "next", "vulns": []}]},
            )

    def test_missing_osv_result_fails(self):
        with self.assertRaisesRegex(ValueError, "one result per"):
            self.audit.osv_advisories(self.packages, lambda *_: {"results": []})

    def test_informational_record_is_retained_in_full(self):
        record = {
            "id": "RUSTSEC-SYNTHETIC",
            "database_specific": {"informational": "unmaintained"},
        }

        def fetch(url, _data=None):
            if url.endswith("querybatch"):
                return {"results": [{"vulns": [{"id": record["id"]}]}]}
            return record

        result = self.audit.osv_advisories(self.packages, fetch)
        self.assertEqual(result["advisories"][record["id"]], record)
        self.assertEqual(result["matches"][0]["package"], self.packages[0])

    def test_node_inventory_keeps_nested_versions_and_tree_errors(self):
        tree = {
            "name": "renovate",
            "version": "1.0",
            "problems": ["missing: synthetic"],
            "dependencies": {
                "example": {
                    "version": "1.0",
                    "dependencies": {
                        "example": {"version": "2.0"},
                    },
                }
            },
        }
        result = self.audit.npm_advisories(tree, lambda _url, data: {"submitted": data})
        self.assertEqual(result["installed_versions"]["example"], ["1.0", "2.0"])
        self.assertEqual(result["tree_problems"], ["missing: synthetic"])


class LiveGateTests(unittest.TestCase):
    def test_help_no_live_and_invalid_contract_do_not_import_aws_sdk(self):
        ordinary_import = builtins.__import__

        def without_sdk(name, *args, **kwargs):
            if name.split(".")[0] in {"boto3", "botocore"}:
                self.fail(f"SDK import before validation: {name}")
            return ordinary_import(name, *args, **kwargs)

        with patch("builtins.__import__", side_effect=without_sdk):
            check = load("query_check", "check.py")
            with (
                patch("sys.argv", ["check.py", "--help"]),
                self.assertRaises(SystemExit) as error,
            ):
                check.main()
            self.assertEqual(error.exception.code, 0)
            with (
                patch("sys.argv", ["check.py"]),
                self.assertRaisesRegex(RuntimeError, "live mode requires"),
            ):
                check.main()
            with self.assertRaises((KeyError, RuntimeError)):
                check.run_live({})


if __name__ == "__main__":
    unittest.main()
