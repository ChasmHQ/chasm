"""pyproject.toml says what sevm actually needs.

Two rules, both of which used to be true only by accident. Nothing may ask for the
`tester` extra: `web3[tester]` reaches `eth-hash[pysha3]` -> safe-pysha3, whose wheels
are x86_64-only, so on arm64 Linux, Apple silicon or Windows it builds a C extension
from source and `uv sync` fails on a machine without a toolchain. And every third-party
package `src/sevm` imports must be declared, rather than arriving under some other
package's dependency tree, where a release that drops it takes sevm with it.
"""

from __future__ import annotations

import ast
import importlib.metadata
import os
import re
import sys

import pytest
from packaging.requirements import Requirement

import sevm

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


def _third_party_imports() -> dict[str, set[str]]:
    """Top-level module imported by `src/sevm` -> the files importing it."""
    src = os.path.dirname(os.path.abspath(sevm.__file__))
    found: dict[str, set[str]] = {}
    for root, _dirs, files in os.walk(src):
        for name in files:
            if not name.endswith(".py"):
                continue
            path = os.path.join(root, name)
            with open(path, encoding="utf-8") as fh:
                tree = ast.parse(fh.read())
            for node in ast.walk(tree):
                modules: list[str] = []
                if isinstance(node, ast.Import):
                    modules = [alias.name.split(".")[0] for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                    modules = [node.module.split(".")[0]]
                for module in modules:
                    if module != "sevm" and module not in sys.stdlib_module_names:
                        found.setdefault(module, set()).add(path)
    return found


def _canonical(name: str) -> str:
    """PEP 503 name, so `eth_abi` and `eth-abi` compare equal."""
    return re.sub(r"[-_.]+", "-", name).lower()


def test_every_imported_package_is_declared():
    declared = {_canonical(Requirement(dep).name) for dep in _dependencies()}
    # `eth` ships in py-evm and `solcx` in py-solc-x, so the import name is not the
    # distribution name; the installed metadata is what maps one to the other.
    provided_by = importlib.metadata.packages_distributions()
    for module, files in sorted(_third_party_imports().items()):
        distributions = {_canonical(name) for name in provided_by.get(module, [])}
        assert distributions & declared, (
            f"{module} is imported by {', '.join(sorted(files))} but no dependency in "
            f"pyproject.toml provides it (installed as: {sorted(distributions) or '?'})"
        )
