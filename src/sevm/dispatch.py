"""The external dispatcher, read back out of runtime bytecode.

`b withdraw` breaks in the *body*, but the pc a call actually arrives at is the
selector's external wrapper, and the wrapper is not the implementation: solc emits it as
`push return tag; decode the arguments; push the implementation tag; JUMP`. This module
walks that shape to recover the tag, which is the pc every caller of that function
converges on, including one arriving through a proxy or with no source at all.

Analysis is static and bytecode-only, so it works on any deployed contract, matching what
`cast disassemble` plus a hand-written scan gives you.
"""

from __future__ import annotations

from dataclasses import dataclass

from .disasm import Disassembly, Instruction
from .srcmap import PUSH1, PUSH32

PUSH4 = PUSH1 + 3

# A selector fits in four bytes; a wider push is holding a constant. Narrower ones are
# accepted because a selector with leading zero bytes can be pushed short.
_SELECTOR_PUSHES = range(PUSH1, PUSH4 + 1)

_TERMINATORS = frozenset({"STOP", "RETURN", "REVERT", "INVALID", "SELFDESTRUCT"})


@dataclass(frozen=True)
class Entry:
    """One selector's route into the contract."""

    selector: int
    dispatch_pc: int  # where the selector is compared
    wrapper_pc: int  # the JUMPDEST the comparison jumps to
    internal_pc: int | None  # the implementation tag the wrapper calls
    return_pc: int | None  # where the wrapper resumes once it returns
    reason: str = ""  # why internal_pc is None, when it is

    @property
    def selector_hex(self) -> str:
        return f"0x{self.selector:08x}"

    @property
    def entry_pc(self) -> int:
        """The best address to break on: the implementation if we found one."""
        return self.wrapper_pc if self.internal_pc is None else self.internal_pc


def _is_push(ins: Instruction) -> bool:
    return PUSH1 <= ins.opcode <= PUSH32


def find_selector(dis: Disassembly, selector: int) -> Entry | None:
    """Locate `selector` in the dispatcher and follow its wrapper to the implementation.

    The dispatcher is a run of `PUSH4 <selector>; EQ; PUSH2 <wrapper>; JUMPI` tests, with
    a `DUP` before the `EQ` in older codegen and `GT`/`LT` nodes between them once solc
    binary-searches. Only the `EQ` leaves name a wrapper, so matching on `EQ` skips the
    search tree without having to understand it.
    """
    ins = dis.instructions
    for n, cmp_push in enumerate(ins):
        if cmp_push.opcode not in _SELECTOR_PUSHES or cmp_push.operand != selector:
            continue
        rest = ins[n + 1 : n + 5]
        if rest and rest[0].mnemonic.startswith("DUP"):
            rest = rest[1:]
        if len(rest) < 3 or rest[0].mnemonic != "EQ":
            continue
        tag, jumpi = rest[1], rest[2]
        if not _is_push(tag) or jumpi.mnemonic != "JUMPI":
            continue
        wrapper = tag.operand
        if wrapper is None or wrapper not in dis.jumpdests:
            continue
        internal, ret, reason = _follow_wrapper(dis, wrapper)
        return Entry(selector, cmp_push.pc, wrapper, internal, ret, reason)
    return None


def entries(dis: Disassembly) -> list[Entry]:
    """Every selector the dispatcher tests, in bytecode order.

    Best-effort, for code with no ABI to hand: it reads the comparison shape rather than
    the dispatcher's bounds, so it takes only `PUSH4` to keep a `PUSH1 0x01; DUP2; EQ`
    inside a memory-copy helper out of the list. With an artifact, drive it from
    `method_identifiers` and call `find_selector` instead.
    """
    found: dict[int, Entry] = {}
    for n, ins in enumerate(dis.instructions):
        if ins.opcode != PUSH4 or ins.operand is None:
            continue
        after = dis.instructions[n + 1 : n + 3]
        if after and after[0].mnemonic.startswith("DUP"):
            after = after[1:]
        if not after or after[0].mnemonic != "EQ":
            continue
        if ins.operand in found:
            continue
        entry = find_selector(dis, ins.operand)
        if entry is not None:
            found[ins.operand] = entry
    return list(found.values())


def _follow_wrapper(dis: Disassembly, wrapper: int) -> tuple[int | None, int | None, str]:
    """Walk the wrapper from its JUMPDEST to the tag it calls.

    Two steps, both keyed on the return tag. The wrapper pushes it first, before any
    argument decoding, and the implementation is whichever call comes back to it: the
    decoder calls that precede it return to their own tags, and the return-value encoder
    that follows is reached after it. Taking the last call before the tag instead would
    read a payable guard's revert stub as the end of the wrapper.
    """
    ret, index = _return_tag(dis, wrapper)
    if ret is None:
        return None, None, "no return tag; the body may be inlined into the wrapper"
    ins = dis.instructions
    while index < len(ins) - 2 and ins[index].pc < ret:
        target, jump, after = ins[index], ins[index + 1], ins[index + 2]
        if (
            _is_push(target)
            and jump.mnemonic == "JUMP"
            and after.pc == ret
            and target.operand in dis.jumpdests
        ):
            return target.operand, ret, ""
        index += 1
    return None, ret, "the wrapper makes no call; the body may be inlined"


def _return_tag(dis: Disassembly, wrapper: int) -> tuple[int | None, int]:
    """The first tag the wrapper pushes as a plain value, and where it sits.

    A non-payable function opens with `CALLVALUE; DUP1; ISZERO; PUSH <ok>; JUMPI` over a
    revert stub, so the walk takes that branch rather than falling into the stub and
    reading its REVERT as the end of the wrapper.
    """
    index = dis.index_of(wrapper)
    if index is None:
        return None, 0
    ins = dis.instructions
    index += 1
    seen: set[int] = set()
    while index < len(ins):
        here = ins[index]
        if here.pc in seen:
            break
        seen.add(here.pc)
        nxt = ins[index + 1] if index + 1 < len(ins) else None
        if _is_push(here) and here.operand in dis.jumpdests:
            if nxt is not None and nxt.mnemonic == "JUMPI":
                target = dis.index_of(here.operand)
                index = index + 2 if target is None else target
                continue
            return here.operand, index
        if here.mnemonic in _TERMINATORS or here.mnemonic == "JUMP":
            break
        index += 1
    return None, 0
