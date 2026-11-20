"""How wide a local is on the stack.

Legacy (non-via-IR) codegen uses one word per value, one pointer per memory reference, one
slot per storage reference. `None` for anything unrecognised is deliberate: an unknown
width would shift every slot below it, so it is better to propagate `<unavailable>` than to
misread the neighbours.
"""

from __future__ import annotations

import re


def stack_slots(type_string: str, location: str) -> int | None:
    """How many stack slots a local of this type occupies, or None if we cannot say.

    Legacy (non-via-IR) codegen uses one word per value, one pointer per memory
    reference, one slot per storage reference. Exceptions: calldata dynamic types carry
    offset+length, external function types carry address+selector. None is deliberate for
    anything unrecognised, since an unknown width would shift every slot below it; better
    to propagate `<unavailable>` than misread the neighbours.
    """
    t = re.sub(r"\s+", " ", (type_string or "").strip())
    if not t:
        return None
    if t.startswith("function "):
        # `function () external ...` is (address, selector); internal is a code offset.
        return 2 if " external" in t else 1
    if location == "calldata":
        # Dynamic calldata arrays, `string`/`bytes`: pointer plus length.
        if t.endswith("[]") or t in ("string", "bytes"):
            return 2
        return 1
    return 1


def _base_type(type_string: str) -> str:
    """Strip solc's location and pointer-ness suffixes off a type string."""
    t = re.sub(r"\s+", " ", (type_string or "").strip())
    return re.sub(r"\s+(storage|memory|calldata)(\s+(ref|pointer|slice))?$", "", t)


def _is_value_type(type_string: str) -> bool:
    t = _base_type(type_string)
    if t in ("bool", "address", "address payable", "string", "bytes"):
        return t not in ("string", "bytes")
    if t.startswith(("uint", "int")) and "[" not in t:
        return True
    if re.fullmatch(r"bytes([1-9]|[12][0-9]|3[0-2])", t):
        return True
    return t.startswith(("contract ", "enum "))
