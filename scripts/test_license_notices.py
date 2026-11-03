"""Check extraction boundaries for dependency copyright headers."""

import importlib.util
import unittest
from pathlib import Path


class LicenseHeaderTests(unittest.TestCase):
    def test_copyright_header_does_not_copy_api_example_keys(self):
        spec = importlib.util.spec_from_file_location(
            "licenses", Path(__file__).with_name("check-licenses.py")
        )
        licenses = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(licenses)
        header = "// Copyright Example Authors.\n// SPDX-License-Identifier: MIT\n"
        self.assertEqual(
            licenses.preamble(
                header + "\n//! API example: key = invented-public-sample\nfn main() {}"
            ),
            header,
        )
        self.assertEqual(
            licenses.preamble(
                "/* Copyright Example Authors.\n * License terms remain here.\n */\nfn main() {}"
            ),
            "/* Copyright Example Authors.\n * License terms remain here.\n */\n",
        )
