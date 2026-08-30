"""The dependency set installs without a compiler.

`web3[tester]` reaches `eth-hash[pysha3]` -> safe-pysha3, whose wheels are x86_64-only:
on arm64 Linux, Apple silicon or Windows it builds a C extension from source, so
`uv sync` fails outright on a machine without a toolchain. sevm names the eth-tester
stack itself to keep that extra out, and web3's own `eth-hash[pycryptodome]` (wheels
everywhere) is the keccak backend that remains. Both halves are asserted here because
either one silently reintroduces the other's failure.
"""

from __future__ import annotations

import os
import re

import pytest
from packaging.requirements import Requirement

try:  # 3.11+; sevm supports 3.10
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - 3.10 only
    tomllib = None

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PYPROJECT = os.path.join(ROOT, "pyproject.toml")
LOCK = os.path.join(ROOT, "uv.lock")


def _dependencies() -> list[str]:
    if tomllib is None:
        pytest.skip("needs tomllib (3.11+)")
    if not os.path.isfile(PYPROJECT):
        pytest.skip("not running from a source checkout")
    with open(PYPROJECT, "rb") as fh:
        return tomllib.load(fh)["project"]["dependencies"]


def test_no_dependency_asks_for_the_tester_extra():
    for dep in _dependencies():
        requirement = Requirement(dep)
        assert "tester" not in requirement.extras, (
            f"{requirement.name}[tester] pulls eth-hash[pysha3]; name eth-tester and "
            "py-evm directly instead"
        )


def test_the_lock_holds_no_safe_pysha3():
    if not os.path.isfile(LOCK):
        pytest.skip("not running from a source checkout")
    with open(LOCK, encoding="utf-8") as fh:
        names = set(re.findall(r'^name = "(.+)"$', fh.read(), re.MULTILINE))
    assert "safe-pysha3" not in names


def test_keccak_runs_on_the_pycryptodome_backend():
    from eth_hash.utils import auto_choose_backend
    from eth_utils import keccak

    assert type(auto_choose_backend()).__module__ == "eth_hash.backends.pycryptodome"
    assert (
        keccak(b"").hex()
        == "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    )
