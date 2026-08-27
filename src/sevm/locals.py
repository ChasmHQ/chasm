"""Local variables: the AST says what they are, the run says where they are.

solc's standard JSON has no location info for locals, but the AST carries every
`VariableDeclaration` (name, type, data location, source range, enclosing scope id).
Only the stack slot is missing, recoverable at run time from the source map (same
technique Truffle and Remix use).

Two observations, verified against real traces in `research/spikes/`:

  1. **Frame base.** An internal Solidity call is an `i`-marked JUMP. At the JUMPDEST it
     lands on, the stack holds the caller's frame plus the return label plus the
     arguments, so parameters occupy the slots directly below entry height.

  2. **Allocation site.** The instruction that allocates a local is attributed to the
     `VariableDeclaration` node's own source range (`uint256 fee`), distinct from the
     enclosing statement's range (`uint256 fee = _fee(amount)`). Stack height immediately
     before that instruction is the local's absolute position, which doesn't move while
     the frame lives.

`session.py` does the observing; this module owns the static half (what's declared and
where it's in scope) and the decoding half (a stack word plus a type becomes a value).

Nothing here guesses: a local whose slot was never observed, or outside its scope,
reports `<unavailable>` with a reason. A plausible wrong number is worse than a gap.
"""

from __future__ import annotations

import re
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

# Declaration kinds, in the order solc pushes them onto the stack.
KIND_PARAM = "param"
KIND_RETURN = "return"
KIND_LOCAL = "local"

# Scope-owning AST nodes. A local's `scope` field names one of these (or a
# FunctionDefinition / ModifierDefinition for parameters).
_SCOPE_NODES = {
    "Block",
    "ForStatement",
    "UncheckedBlock",
    "FunctionDefinition",
    "ModifierDefinition",
    "TryCatchClause",
}

_FUNCTION_NODES = {"FunctionDefinition", "ModifierDefinition"}

_IDENTIFIER = re.compile(r"(?<![\w.$])([A-Za-z_$][\w$]*)")


def _parse_src(src: str) -> tuple[int, int, int]:
    parts = (src or "0:0:-1").split(":")
    while len(parts) < 3:
        parts.append("-1")
    return int(parts[0]), int(parts[1]), int(parts[2])


# ==================================================================
# stack shape
# ==================================================================


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


# ==================================================================
# the static half: what is declared, and where it is visible
# ==================================================================


@dataclass(frozen=True)
class LocalVar:
    """One `VariableDeclaration` from the AST, with its scope resolved."""

    ast_id: int
    name: str  # "" for an unnamed return parameter
    type_string: str
    location: str  # default | memory | storage | calldata
    kind: str  # param | return | local
    file_id: int
    start: int
    length: int
    scope_start: int
    scope_end: int
    statement_start: int = -1  # enclosing VariableDeclarationStatement, when there is one
    statement_end: int = -1
    function_id: int = -1
    index: int = 0  # declaration order within the function

    @property
    def end(self) -> int:
        return self.start + self.length

    @property
    def slots(self) -> int | None:
        return stack_slots(self.type_string, self.location)

    @property
    def display_type(self) -> str:
        base = _base_type(self.type_string)
        return (
            f"{base} {self.location}"
            if self.location in ("memory", "storage", "calldata")
            else base
        )

    def visible_at(self, offset: int) -> bool:
        """gdb's rule: declared before here, and here is still inside its scope."""
        if not (self.scope_start <= offset < self.scope_end):
            return False
        if self.kind in (KIND_PARAM, KIND_RETURN):
            return True
        return offset >= self.start


@dataclass
class FunctionLocals:
    """Every declaration belonging to one function or modifier."""

    function_id: int
    params: list[LocalVar] = field(default_factory=list)
    returns: list[LocalVar] = field(default_factory=list)
    body: list[LocalVar] = field(default_factory=list)

    @property
    def all(self) -> list[LocalVar]:
        return self.params + self.returns + self.body


class LocalsIndex:
    """Every local declaration in the project, indexed three ways: by function (frame
    layout), by exact source range (spot the allocating instruction in the source map),
    and by source offset ("what is in scope here").
    """

    def __init__(self, asts: dict[str, dict]) -> None:
        self.by_function: dict[int, FunctionLocals] = {}
        self.by_range: dict[tuple[int, int, int], LocalVar] = {}
        self.modifier_ids: set = set()
        self._scopes: dict[int, tuple[int, int, int]] = {}  # ast id -> (file, start, end)
        for ast in asts.values():
            self._index_scopes(ast)
        for ast in asts.values():
            self._walk(ast, function=None, statement=None)
        self._finalise()

    def _finalise(self) -> None:
        """Put every list in the order solc pushes them: params, returns, then locals.

        The AST walk visits a function's body before its parameter list, so insertion
        order isn't allocation order, and allocation order is what frame layout needs.
        """
        for entry in self.by_function.values():
            entry.params.sort(key=lambda v: v.start)
            entry.returns.sort(key=lambda v: v.start)
            entry.body.sort(key=lambda v: v.start)
            for position, var in enumerate(entry.all):
                object.__setattr__(var, "index", position)

    # -- construction -------------------------------------------------------

    def _index_scopes(self, node: Any) -> None:
        """Pass one: every scope-owning node's source range, keyed by its AST id.

        A declaration names its scope by id, not by range, so ranges must be collected
        before declarations can be placed.
        """
        if isinstance(node, list):
            for item in node:
                self._index_scopes(item)
            return
        if not isinstance(node, dict):
            return
        if (
            node.get("nodeType") in _SCOPE_NODES
            or node.get("nodeType") == "ContractDefinition"
        ):
            start, length, file_id = _parse_src(node.get("src", ""))
            self._scopes[int(node.get("id", -1))] = (file_id, start, start + length)
        for value in node.values():
            if isinstance(value, (dict, list)):
                self._index_scopes(value)

    def _walk(self, node: Any, function: dict | None, statement: dict | None) -> None:
        if isinstance(node, list):
            for item in node:
                self._walk(item, function, statement)
            return
        if not isinstance(node, dict):
            return

        node_type = node.get("nodeType")
        if node_type in _FUNCTION_NODES:
            function = node
            self._add_function(node)
            if node_type == "ModifierDefinition":
                self.modifier_ids.add(int(node.get("id", -1)))
        elif node_type == "VariableDeclarationStatement":
            statement = node
        elif node_type == "VariableDeclaration" and function is not None:
            if not node.get("stateVariable"):
                self._add_local(node, function, statement)

        for value in node.values():
            if isinstance(value, (dict, list)):
                self._walk(value, function, statement)

    def _add_function(self, node: dict) -> None:
        fid = int(node.get("id", -1))
        self.by_function.setdefault(fid, FunctionLocals(function_id=fid))

    def _add_local(self, node: dict, function: dict, statement: dict | None) -> None:
        fid = int(function.get("id", -1))
        entry = self.by_function.setdefault(fid, FunctionLocals(function_id=fid))
        if any(v.ast_id == int(node.get("id", -1)) for v in entry.all):
            return

        start, length, file_id = _parse_src(node.get("src", ""))
        scope_id = int(node.get("scope", -1))
        scope = self._scopes.get(scope_id)
        if scope is None or scope[0] != file_id:
            # A declaration we cannot place has no trustworthy visibility rule, so it is
            # dropped rather than shown with a scope that might be wrong.
            return

        kind = KIND_LOCAL
        params = (function.get("parameters") or {}).get("parameters") or []
        returns = (function.get("returnParameters") or {}).get("parameters") or []
        node_id = int(node.get("id", -1))
        if any(int(p.get("id", -2)) == node_id for p in params):
            kind = KIND_PARAM
        elif any(int(r.get("id", -2)) == node_id for r in returns):
            kind = KIND_RETURN

        stmt_start, stmt_end = -1, -1
        if statement is not None and kind == KIND_LOCAL:
            s_start, s_length, _ = _parse_src(statement.get("src", ""))
            stmt_start, stmt_end = s_start, s_start + s_length

        var = LocalVar(
            ast_id=node_id,
            name=node.get("name") or "",
            type_string=(node.get("typeDescriptions") or {}).get("typeString") or "",
            location=node.get("storageLocation") or "default",
            kind=kind,
            file_id=file_id,
            start=start,
            length=length,
            scope_start=scope[1],
            scope_end=scope[2],
            statement_start=stmt_start,
            statement_end=stmt_end,
            function_id=fid,
            index=len(entry.all),
        )
        if kind == KIND_PARAM:
            entry.params.append(var)
        elif kind == KIND_RETURN:
            entry.returns.append(var)
        else:
            entry.body.append(var)
        # Parameters share their range with nothing; a body declaration is what the
        # allocating instruction points at.
        self.by_range[(file_id, start, length)] = var

    # -- lookup -------------------------------------------------------------

    def for_function(self, function_id: int) -> FunctionLocals | None:
        return self.by_function.get(function_id)

    def at_range(self, file_id: int, start: int, length: int) -> LocalVar | None:
        return self.by_range.get((file_id, start, length))

    def owned_by_modifier(self, var: LocalVar) -> bool:
        """A modifier's body is inlined into the function it decorates, so its locals
        live in the same EVM frame/stack but belong to the ModifierDefinition, not the
        function's own layout.
        """
        return var.function_id in self.modifier_ids

    def by_ast_id(self, ast_id: int) -> LocalVar | None:
        for entry in self.by_function.values():
            for var in entry.all:
                if var.ast_id == ast_id:
                    return var
        return None

    def visible(self, function_id: int, offset: int) -> list[LocalVar]:
        """In-scope declarations at `offset`, innermost shadowing outermost."""
        entry = self.by_function.get(function_id)
        if entry is None:
            return []
        chosen: dict[str, LocalVar] = {}
        unnamed: list[LocalVar] = []
        for var in entry.all:
            if not var.visible_at(offset):
                continue
            if not var.name:
                unnamed.append(var)
                continue
            previous = chosen.get(var.name)
            # A tighter scope shadows a wider one, which is what Solidity does and what
            # the eval injector must reproduce or it will read the wrong variable.
            if previous is None or (var.scope_end - var.scope_start) <= (
                previous.scope_end - previous.scope_start
            ):
                chosen[var.name] = var
        out = list(chosen.values()) + unnamed
        out.sort(key=lambda v: v.index)
        return out


def declaration_pcs(pc_map: Any, index: LocalsIndex) -> dict[int, LocalVar]:
    """pc -> the declaration that instruction allocates.

    Built once per code object so `session.py` can do a cheap dict lookup per opcode,
    running on every instruction.

    Parameters are excluded: they're pushed by the caller, so their slots come from the
    frame base instead, and solc's ABI decoder sometimes attributes an instruction to a
    parameter's declaration range, which would record a position belonging to no frame.
    """
    out: dict[int, LocalVar] = {}
    if pc_map is None:
        return out
    for pc in pc_map.pcs:
        entry = pc_map.entry_at(pc)
        if entry is None or entry.is_generated:
            continue
        var = index.at_range(entry.file_id, entry.start, entry.length)
        if var is not None and var.kind in (KIND_LOCAL, KIND_RETURN):
            out.setdefault(pc, var)
    return out


# ==================================================================
# the decoding half: a stack word plus a type becomes a value
# ==================================================================


@dataclass
class LocalValue:
    """One row of `info locals`."""

    name: str
    type_label: str
    display: str
    available: bool = True
    reason: str = ""
    kind: str = KIND_LOCAL
    # Set when the value can be handed to the evaluator as a function argument.
    abi_type: str = ""
    abi_value: Any = None
    words: tuple[int, ...] = ()
    position: int | None = None  # absolute stack position of the first word
    # True only when the stack word *is* the value. A memory or calldata local holds a
    # pointer, so writing its slot corrupts the reference instead of changing the value.
    writable: bool = False

    @property
    def bindable(self) -> bool:
        return self.available and bool(self.name) and bool(self.abi_type)


MemoryReader = Callable[[int, int], bytes]


def _word(value: int) -> bytes:
    return int(value).to_bytes(32, "big")


def _format_int(value: int, type_label: str) -> str:
    if type_label.startswith("uint") and 10**15 <= value < 10**27:
        whole, frac = divmod(value, 10**18)
        if frac == 0:
            return f"{value} ({whole} ether)"
        return f"{value} ({value / 10**18:.6f} ether)"
    return str(value)


def decode_value_type(word: int, type_string: str) -> tuple[str, str, Any]:
    """(display, abi_type, python value) for a type that lives entirely in one word."""
    t = _base_type(type_string)
    raw = _word(word)
    if t == "bool":
        val = word != 0
        return ("true" if val else "false", "bool", val)
    if t in ("address", "address payable") or t.startswith("contract "):
        addr = "0x" + raw[-20:].hex()
        return (addr, "address", addr)
    match = re.fullmatch(r"bytes([1-9]|[12][0-9]|3[0-2])", t)
    if match:
        size = int(match.group(1))
        chunk = raw[:size]  # bytesN is left-aligned on the stack
        return ("0x" + chunk.hex(), t, chunk)
    if t.startswith("enum "):
        return (str(word), "uint256", word)
    if t.startswith("int") and not t.startswith("uint"):
        signed = int.from_bytes(raw, "big", signed=True)
        width = t[3:] or "256"
        return (str(signed), f"int{width}", signed)
    width = (t[4:] or "256") if t.startswith("uint") else "256"
    return (_format_int(word, t), f"uint{width}", word)


def _read_memory_bytes(read: MemoryReader, pointer: int) -> bytes:
    length = int.from_bytes(read(pointer, 32), "big")
    if length > 1 << 20:  # a pointer into uninitialised memory, not a real string
        raise ValueError(f"implausible length {length}")
    return read(pointer + 32, length)


def _decode_memory(
    read: MemoryReader, pointer: int, type_string: str, depth: int = 0
) -> tuple[str, str, Any]:
    """Decode a memory reference type by following its pointer.

    Solidity's memory layout is uniform: a dynamic array/string is a length word
    followed by elements; a struct/fixed array is its members in order; every member is
    one word holding either a value or another pointer.
    """
    t = _base_type(type_string)
    if depth > 3:
        raise ValueError("nested too deeply to display")

    if t in ("string", "bytes"):
        data = _read_memory_bytes(read, pointer)
        if t == "string":
            try:
                text = data.decode("utf-8")
                return (f'"{text}"', "string", text)
            except UnicodeDecodeError:
                pass
        return ("0x" + data.hex(), "bytes", data)

    array = re.fullmatch(r"(.+)\[(\d*)\]", t)
    if array:
        element, fixed = array.group(1).strip(), array.group(2)
        if fixed:
            count, base = int(fixed), pointer
        else:
            count = int.from_bytes(read(pointer, 32), "big")
            base = pointer + 32
        if count > 256:
            raise ValueError(f"{count} elements is too many to display")
        displays, values = [], []
        for i in range(count):
            word = int.from_bytes(read(base + 32 * i, 32), "big")
            if _is_value_type(element):
                shown, abi_element, value = decode_value_type(word, element)
            else:
                shown, abi_element, value = _decode_memory(read, word, element, depth + 1)
            displays.append(shown)
            values.append(value)
        abi_element = decode_value_type(0, element)[1] if _is_value_type(element) else ""
        suffix = f"[{fixed}]" if fixed else "[]"
        abi_type = f"{abi_element}{suffix}" if abi_element else ""
        return (f"[{count} items] [" + ", ".join(displays) + "]", abi_type, values)

    raise ValueError(f"cannot decode {t} from memory yet")


def read_local(
    var: LocalVar,
    words: Sequence[int],
    read_memory: MemoryReader,
) -> LocalValue:
    """Turn the raw stack words of one local into a displayable, bindable value."""
    name = var.name or f"<{var.kind}>"
    label = var.display_type

    if var.location == "storage":
        slot = words[0]
        shown = f"0x{slot:x}" if slot > 0xFFFF else str(slot)
        return LocalValue(
            name=name,
            type_label=label,
            display=f"<storage pointer -> slot {shown}>",
            available=True,
            reason="storage pointers are not dereferenced; index the state variable instead",
            kind=var.kind,
            words=tuple(words),
        )

    if var.location == "calldata":
        return LocalValue(
            name=name,
            type_label=label,
            display="<calldata reference: " + ", ".join(f"0x{w:x}" for w in words) + ">",
            available=True,
            reason="calldata references are not decoded; `info args` decodes the call's arguments",
            kind=var.kind,
            words=tuple(words),
        )

    if _is_value_type(var.type_string):
        display, abi_type, value = decode_value_type(words[0], var.type_string)
        return LocalValue(
            name=name,
            type_label=label,
            display=display,
            kind=var.kind,
            abi_type=abi_type,
            abi_value=value,
            words=tuple(words),
            writable=True,
        )

    if var.location == "memory":
        try:
            display, abi_type, value = _decode_memory(
                read_memory, words[0], var.type_string
            )
        except Exception as exc:
            return LocalValue(
                name=name,
                type_label=label,
                display="<unavailable>",
                available=False,
                reason=str(exc),
                kind=var.kind,
                words=tuple(words),
            )
        return LocalValue(
            name=name,
            type_label=label,
            display=display,
            kind=var.kind,
            abi_type=abi_type,
            abi_value=value,
            words=tuple(words),
        )

    return LocalValue(
        name=name,
        type_label=label,
        display="<unavailable>",
        available=False,
        reason=f"no decoder for {label}",
        kind=var.kind,
        words=tuple(words),
    )


def referenced_names(expression: str) -> set:
    """Identifiers an expression mentions, ignoring member accesses like `a.b`."""
    return {m.group(1) for m in _IDENTIFIER.finditer(expression)}
