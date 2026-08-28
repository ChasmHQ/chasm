"""Compiling the test contracts, and matching running code back to an artifact."""

from __future__ import annotations

from sevm.compile.model import _strip_metadata


def test_project_compiles_both_contracts(proj):
    assert {"Bank.sol:Bank", "Bank.sol:Callee"} <= set(proj.artifacts)
    bank = proj.artifact("Bank")
    assert bank.deployed_bytecode and bank.deployed_source_map
    assert bank.storage_layout["storage"]
    assert "deposit()" in bank.method_identifiers


def test_source_range_targets_the_right_contract(proj):
    """A file with two contracts must not resolve both to the last closing brace."""
    text = proj.sources["Bank.sol"].text
    for name in ("Bank", "Callee"):
        start, end = proj.artifact(name).source_range
        assert text[start:].startswith(f"contract {name} ")
        assert text[end - 1] == "}"
    assert proj.artifact("Bank").source_range[1] < proj.artifact("Callee").source_range[0]


def test_artifact_lookup_by_runtime_code(proj):
    bank = proj.artifact("Bank")
    assert proj.artifact_for_code(bank.deployed_bytecode) is bank
    assert proj.artifact_for_code(b"\x60\x00") is None
    assert proj.artifact_for_code(b"") is None


def test_metadata_stripping_shortens_code(proj):
    code = proj.artifact("Bank").deployed_bytecode
    assert len(_strip_metadata(code)) < len(code)
    assert _strip_metadata(b"\x00") == b"\x00"


def test_selectors_round_trip(proj):
    selectors = proj.artifact("Bank").selectors
    assert "deposit()" in selectors.values()
    assert all(len(sel) == 4 for sel in selectors)
