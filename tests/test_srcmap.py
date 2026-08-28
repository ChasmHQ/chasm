"""Source maps, the line index, the disassembler, and the AST function index."""

from __future__ import annotations

from harness import (
    line_indexes,
    line_of,
)

from sevm.disasm import CALL_OPCODES, OPCODES, Disassembly, disassemble
from sevm.frames import FunctionIndex
from sevm.srcmap import (
    LineIndex,
    PcMap,
    instruction_pcs,
    parse_source_map,
)


def test_parse_source_map_fills_elided_fields():
    entries = parse_source_map("1:2:0:i:0;;3:::o;::-1")
    assert len(entries) == 4
    assert (entries[0].start, entries[0].length, entries[0].file_id, entries[0].jump) == (
        1,
        2,
        0,
        "i",
    )
    assert entries[1] == entries[0]  # an empty chunk repeats everything
    assert entries[2].start == 3 and entries[2].length == 2 and entries[2].jump == "o"
    assert entries[3].file_id == -1 and entries[3].is_generated

    # A malformed field must degrade, not crash: one bad entry cannot blind the debugger.
    assert parse_source_map("1:2:0:-:0;x:y:z:-:0")[1].start == 1
    assert parse_source_map("") == []


def test_instruction_pcs_skips_push_immediates():
    # PUSH1 0x01, PUSH0, PUSH2 0xdead, STOP
    code = bytes([0x60, 0x01, 0x5F, 0x61, 0xDE, 0xAD, 0x00])
    assert instruction_pcs(code) == [0, 2, 3, 6]


def test_disassembler_handles_push0_and_truncated_push():
    code = bytes([0x5F, 0x60, 0x01, 0x7F]) + b"\x11" * 4
    ins = disassemble(code)
    assert ins[0].mnemonic == "PUSH0" and ins[0].immediate is None
    assert ins[1].mnemonic == "PUSH1" and ins[1].operand == 1
    assert ins[2].mnemonic == "PUSH32"
    assert len(ins[2].immediate) == 32  # padded, not dropped


def test_opcode_table_is_complete_enough():
    assert OPCODES[0x00] == "STOP"
    assert OPCODES[0x55] == "SSTORE"
    assert OPCODES[0x5F] == "PUSH0"
    assert {0xF1, 0xF4, 0xFA, 0xF0} <= CALL_OPCODES


def test_line_index_maps_offsets_both_ways():
    index = LineIndex("alpha\nbeta\ngamma")
    assert index.line_col(0) == (1, 1)
    assert index.line_col(6) == (2, 1)
    assert index.line_text(3) == "gamma"
    assert index.line_range(6, 4) == (2, 2)


def test_pcmap_resolves_lines_and_back(proj):
    art = proj.artifact("Bank")
    pcmap = PcMap(art.deployed_bytecode, art.deployed_source_map, line_indexes(proj))
    executable = pcmap.executable_lines(0)
    assert executable, "no executable lines found"

    deposit_line = line_of(proj, "_credit(msg.sender, msg.value);")
    pc = pcmap.first_pc_for_line(0, deposit_line)
    assert pc is not None
    assert pcmap.at(pc).line == deposit_line

    # A blank line snaps forward to the next line that actually has code.
    blank = line_of(proj, "event Deposited")
    assert pcmap.nearest_executable_line(0, blank) >= blank


def test_pcmap_marks_internal_jumps(proj):
    art = proj.artifact("Bank")
    pcmap = PcMap(art.deployed_bytecode, art.deployed_source_map, line_indexes(proj))
    jumps = {pcmap.at(pc).jump for pc in pcmap.pcs if pcmap.at(pc)}
    assert "i" in jumps and "o" in jumps, (
        "internal call markers are what step/next rely on"
    )


def test_disassembly_index(proj):
    art = proj.artifact("Bank")
    disasm = Disassembly(art.deployed_bytecode)
    assert len(disasm) > 100
    assert disasm.jumpdests
    first_dest = min(disasm.jumpdests)
    assert disasm.is_valid_jumpdest(first_dest)
    assert not disasm.is_valid_jumpdest(first_dest + 1_000_000)
    assert len(disasm.window(first_dest, 3, 5)) <= 8


def test_function_index_names_and_ranges(proj):
    index = FunctionIndex(proj.asts)
    names = {fn.display_name for fn in index.functions}
    assert {"Bank.deposit", "Bank._fee", "Bank._credit", "Callee.receiveValue"} <= names

    fee = index.find("_fee")[0]
    assert fee.contract == "Bank"
    assert fee.visibility == "internal"
    assert fee.parameters == (("uint256", "amount"),)
    assert index.at_offset(fee.file_id, fee.start + 5) is fee


def test_function_index_finds_modifiers_and_constructor(proj):
    index = FunctionIndex(proj.asts)
    kinds = {fn.kind for fn in index.functions}
    assert "modifier" in kinds and "constructor" in kinds
