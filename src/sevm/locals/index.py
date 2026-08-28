"""The static half: what solc declared, and where each name is in scope.

solc's standard JSON has no location info for locals, but the AST carries every
`VariableDeclaration` (name, type, data location, source range, enclosing scope id). This
module turns that into a lookup the running session can consult per instruction.

The allocation site is the load-bearing detail: the instruction that allocates a local is
attributed to the `VariableDeclaration`'s own source range (`uint256 fee`), distinct from
the enclosing statement's (`uint256 fee = _fee(amount)`). `declaration_pcs` builds the
pc -> declaration table the hook uses to spot that instruction in one dict lookup.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any

from .layout import _base_type, stack_slots

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


def referenced_names(expression: str) -> set:
    """Identifiers an expression mentions, ignoring member accesses like `a.b`."""
    return {m.group(1) for m in _IDENTIFIER.finditer(expression)}
