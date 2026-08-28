"""The package layout holds together.

sevm is a tree of small packages, and splitting a module a level deeper silently breaks any
`from .x import y` written inside a function body: the name still resolves at import time
because it is never executed, and only fails when that code path runs. That cost three
debugging rounds during the split, so it is a test now.
"""

from __future__ import annotations

import ast
import importlib
import os
import pkgutil
import re

import pytest

import sevm

SRC = os.path.dirname(os.path.abspath(sevm.__file__))
# __main__ runs the CLI on import, by design.
MODULES = sorted(
    m.name
    for m in pkgutil.walk_packages([SRC], prefix="sevm.")
    if not m.name.endswith("__main__")
)


@pytest.mark.parametrize("name", MODULES)
def test_every_module_imports(name):
    importlib.import_module(name)


def _parse(path):
    with open(path, encoding="utf-8") as fh:
        return ast.parse(fh.read())


def _python_files():
    for root, _dirs, files in os.walk(SRC):
        if "__pycache__" in root:
            continue
        for f in files:
            if f.endswith(".py"):
                yield os.path.join(root, f)


def test_relative_imports_point_at_a_real_module():
    """Catches a `from .x import y` left behind when its module moved a level deeper.

    Deferred (function-body) imports are the dangerous ones: nothing executes them at
    import time, so only running that code path finds them.
    """
    bad = []
    for path in _python_files():
        pkg = os.path.dirname(path)
        siblings = {
            os.path.splitext(f)[0]
            for f in os.listdir(pkg)
            if f.endswith(".py") or os.path.isdir(os.path.join(pkg, f))
        }
        for node in ast.walk(_parse(path)):
            if not isinstance(node, ast.ImportFrom) or node.level != 1 or not node.module:
                continue
            head = node.module.split(".")[0]
            if head not in siblings:
                rel = os.path.relpath(path, SRC)
                bad.append(f"{rel}:{node.lineno}: from .{node.module} (no such sibling)")
    assert not bad, "relative imports that cannot resolve:\n  " + "\n  ".join(bad)


def test_package_docstrings_name_their_modules():
    """Each package `__init__` maps its own modules, so a reader lands in the right file."""
    missing = []
    for root, _dirs, files in os.walk(SRC):
        if "__pycache__" in root or "__init__.py" not in files:
            continue
        init = os.path.join(root, "__init__.py")
        doc = ast.get_docstring(_parse(init)) or ""
        peers = sorted(
            f
            for f in files
            if f.endswith(".py") and f not in ("__init__.py", "__main__.py")
        )
        if len(peers) < 2:
            continue
        for peer in peers:
            if not re.search(rf"\b{re.escape(peer)}\b", doc):
                missing.append(f"{os.path.relpath(init, SRC)} does not mention {peer}")
    assert not missing, "package maps are out of date:\n  " + "\n  ".join(missing)
