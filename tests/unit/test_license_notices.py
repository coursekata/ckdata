"""Check extraction boundaries for dependency copyright headers."""

from scripts import check_licenses


def test_copyright_header_does_not_copy_api_example_keys():
    header = "// Copyright Example Authors.\n// SPDX-License-Identifier: MIT\n"
    assert (
        check_licenses.preamble(
            header + "\n//! API example: key = invented-public-sample\nfn main() {}"
        )
        == header
    )
    assert (
        check_licenses.preamble(
            "/* Copyright Example Authors.\n * License terms remain here.\n */\nfn main() {}"
        )
        == "/* Copyright Example Authors.\n * License terms remain here.\n */\n"
    )
