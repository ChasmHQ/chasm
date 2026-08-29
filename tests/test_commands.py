"""The gdb command surface: verbs, aliases, `info` topics, and `help`."""

from __future__ import annotations

import re

from harness import (
    line_of,
)

from sevm.commands.render import _calldata
from sevm.session import Finished, Paused, StepMode


def test_command_break_and_continue(deposit_debugger):
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "history.push(amount);")
    result = dbg.run(f"b Bank.sol:{line}")
    assert result.ok and "Breakpoint 1" in result.lines[0]
    result = dbg.run("c")
    assert result.resumed
    assert dbg.snap.line == line


def test_command_aliases_match_gdb(deposit_debugger):
    dbg = deposit_debugger
    for alias in ("n", "s", "si", "ni"):
        assert dbg.run(alias).ok
    for alias in ("bt", "where", "i r", "l", "disas"):
        assert dbg.run(alias).ok, alias


def test_command_print_and_value_history(deposit_debugger):
    dbg = deposit_debugger
    assert "$1" in dbg.run("p totalDeposits").lines[0]
    assert "$2" in dbg.run("p feeBps").lines[0]
    result = dbg.run("p $1 + 1")
    assert result.ok and "$3" in result.lines[0]


def test_bare_expression_is_printed(deposit_debugger):
    result = deposit_debugger.run("totalDeposits")
    assert result.ok and "$1" in result.lines[0]


def test_command_convenience_variables(deposit_debugger):
    dbg = deposit_debugger
    for expression in (
        "p $pc",
        "p $gas",
        "p $depth",
        "p $sp",
        "p $storage[1]",
        "p $stack[0]",
    ):
        assert dbg.run(expression).ok, expression


def test_command_examine_memory(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("x/32xb 0x40")
    assert result.ok
    assert any("free memory pointer" in line for line in result.lines)
    assert dbg.run("x/4xg 0x0").ok
    assert dbg.run("x/s 0x80").ok
    assert not dbg.run("x/zz 0x0").ok


def test_command_info_topics(deposit_debugger):
    dbg = deposit_debugger
    for topic in (
        "registers",
        "breakpoints",
        "frame",
        "args",
        "locals",
        "storage",
        "gas",
        "logs",
        "sources",
        "functions",
    ):
        result = dbg.run(f"info {topic}")
        assert result.ok, f"info {topic}: {result.error}"
        assert result.lines, f"info {topic} produced nothing"
    assert not dbg.run("info nonsense").ok


def test_info_frame_colours_the_selector_and_each_argument_word(forward_debugger):
    dbg, calldata = forward_debugger
    line = next(ln for ln in dbg.run("info frame").lines if "calldata" in ln)
    assert f"[bold yellow]0x{calldata[:4].hex()}[/bold yellow]" in line
    # forward(address,uint256): two words, coloured apart.
    assert f"[magenta]{calldata[4:36].hex()}[/magenta]" in line
    assert f"[cyan]{calldata[36:].hex()}[/cyan]" in line


def test_calldata_reports_what_it_truncates():
    assert "empty" in _calldata(b"")
    assert _calldata(b"\xd0\xe3\x0d\xb0") == "[bold yellow]0xd0e30db0[/bold yellow]"
    text = _calldata(b"\x00\x00\x40\xc3" + b"\xab" * 100)
    assert "(+36 bytes)" in text
    assert text.count("[magenta]") == 1 and text.count("[cyan]") == 1
    # A cut mid-word colours only as far as the hex goes.
    mid = _calldata(b"\x00\x00\x40\xc3" + b"\xab" * 64, limit=100)
    assert f"[cyan]{'ab' * 18}[/cyan]" in mid
    assert "(+14 bytes)" in mid


def test_info_locals_names_and_values_the_frames_locals(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("b Bank.sol:46")
    dbg.run("c")
    lines = " ".join(dbg.run("info locals").lines)
    assert "who" in lines and "amount" in lines and "fee" in lines
    # 2 ether at 25 bps.
    assert "5000000000000000" in lines
    assert "2000000000000000000" in lines


def test_info_storage_decodes_names(deposit_debugger):
    lines = " ".join(deposit_debugger.run("info storage").lines)
    assert "totalDeposits" in lines and "sevm-bank" in lines


def test_info_gas_profiles_by_line(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    lines = " ".join(dbg.run("info gas").lines)
    assert "gas by source line" in lines
    assert "gas by opcode" in lines


def test_command_set_var_writes_through_solidity(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("set var totalDeposits = 7 ether").ok
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 7 * 10**18
    assert dbg.run("set var balances[msg.sender] = 3 ether").ok
    assert dbg.session.inspect("evaluate", "balances[msg.sender]").value == 3 * 10**18


def test_command_set_convenience(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("set $gas = 90000").ok
    assert dbg.session.inspect("frame_info")["gas_remaining"] == 90000
    assert dbg.run("set $storage[1] = 5").ok
    assert dbg.session.inspect("read_storage", 1) == 5


def test_command_call_keeps_effects(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("call totalDeposits = 11").ok
    assert dbg.session.inspect("evaluate", "totalDeposits").value == 11


def test_command_display_reevaluates_on_each_stop(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("display totalDeposits").ok
    result = dbg.run("s")
    assert any("totalDeposits" in line for line in result.lines)
    assert dbg.run("undisplay").ok
    assert not dbg.commands.displays


def test_command_frame_selection(deposit_debugger):
    dbg = deposit_debugger
    for _ in range(4):
        if isinstance(dbg.step(StepMode.STEP), Finished):
            break
    assert dbg.run("f 1").ok
    assert dbg.run("up").ok
    assert dbg.run("down").ok
    assert not dbg.run("f 99").ok


def test_command_watch_resolves_a_state_variable(deposit_debugger):
    result = deposit_debugger.run("watch totalDeposits")
    assert result.ok and "Watchpoint" in result.lines[0]


def test_command_watch_resolves_a_mapping_element(deposit_debugger):
    result = deposit_debugger.run("watch balances[msg.sender]")
    assert result.ok and "Watchpoint" in result.lines[0]


def test_command_delete_disable_enable(deposit_debugger):
    dbg = deposit_debugger
    dbg.run("b Bank.sol:52")
    assert dbg.run("disable 1").ok
    assert dbg.run("enable 1").ok
    assert dbg.run("delete 1").ok
    assert not dbg.run("delete 1").ok


def test_command_until(deposit_debugger):
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "history.push(amount);")
    result = dbg.run(f"until Bank.sol:{line}")
    assert result.resumed
    assert dbg.snap.line == line


def test_command_ptype(deposit_debugger):
    assert "uint96" in deposit_debugger.run("ptype feeBps").lines[0]


def test_command_help(deposit_debugger):
    dbg = deposit_debugger
    assert dbg.run("help").lines
    for topic in ("breakpoints", "print", "memory", "mutation", "gas", "locals"):
        assert dbg.run(f"help {topic}").ok, topic
    assert not dbg.run("help nonsense").ok


def test_bad_command_reports_instead_of_raising(deposit_debugger):
    result = deposit_debugger.run("p nonsense_identifier")
    assert not result.ok and result.error


def test_command_quit_flag(deposit_debugger):
    assert deposit_debugger.run("q").quit


def test_markup_in_source_is_escaped(deposit_debugger):
    """Source containing brackets must not be interpreted as Rich markup."""
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "balances[who] += amount - fee;")
    dbg.session.break_at_line("Bank.sol", line)
    dbg.step(StepMode.RUN)
    rendered = " ".join(dbg.commands.describe_stop(dbg.snap))
    assert "\\[who]" in rendered


def test_cheatcode_help_is_generated_from_the_registry(deposit_debugger):
    from sevm.cheatcodes import listing

    specs = listing()
    assert specs
    body = " ".join(deposit_debugger.run("help cheatcodes").lines)
    for spec in specs:
        assert spec.doc, f"{spec.name} has no help line"
        assert f"vm.{spec.signature}" in body, f"{spec.name} is missing from the help"


def test_help_summary_mentions_assembly_and_cheatcodes(deposit_debugger):
    summary = " ".join(deposit_debugger.run("help").lines)
    assert "Assembly" in summary and "mstore" in summary
    assert "cheatcodes" in summary and "vm.warp" in summary
    for topic in ("assembly", "cheatcodes", "asm", "yul", "vm", "foundry"):
        assert deposit_debugger.run(f"help {topic}").ok, topic
    body = " ".join(deposit_debugger.run("help foundry").lines)
    assert "--no-install" in body and "forge-std" in body


def test_unknown_command_reports_instead_of_raising(deposit_debugger):
    """A typo used to escape `execute()` and take the frontend down with it.

    The fallback to "evaluate it as Solidity" sat outside the error handling, so an
    unknown verb threw SessionError straight out: the console died, and the TUI lost the
    worker that clears `busy`, wedging the prompt for the rest of the session.
    """
    dbg = deposit_debugger
    for line in ("blah", "brekpoint 12", "foobar 1 2", "!!!", "@@@ ###"):
        result = dbg.run(line)
        assert not result.ok, line
        assert result.error, line
    # Still usable afterwards: nothing was left in a broken state.
    assert dbg.run("p totalDeposits").ok


def test_unknown_command_says_so_and_suggests_a_verb(deposit_debugger):
    error = deposit_debugger.run("brekpoint 12").error
    assert "undefined command" in error and "brekpoint" in error
    assert "help" in error
    assert "`break`" in error, error


def test_bad_arguments_report_the_word_that_is_not_a_number(deposit_debugger):
    dbg = deposit_debugger
    for line in ("delete xyz", "disable xyz", "enable xyz", "jump nowhere", "frame abc"):
        result = dbg.run(line)
        assert not result.ok, line
        assert "int()" not in (result.error or ""), f"{line}: raw Python error leaked"


def test_a_bare_expression_still_evaluates(deposit_debugger):
    """The undefined-command path must not swallow gdb's print-an-expression behaviour."""
    dbg = deposit_debugger
    assert dbg.run("totalDeposits").ok
    assert dbg.run("balances[msg.sender]").ok


# ==================================================================
# stop echo, registers, memory examination, restart
# ==================================================================


def test_stepi_echoes_machine_state(deposit_debugger):
    """Every stop ends with a machine line: pc, opcode, sp, gas, step."""
    dbg = deposit_debugger
    result = dbg.run("si")
    joined = "\n".join(result.lines)
    assert re.search(r"pc 0x[0-9a-f]+", joined)
    assert re.search(r"sp \d+", joined)
    assert "gas" in joined and "step" in joined


def test_stack_height_change_is_surfaced(deposit_debugger):
    """A step that changes the stack height (POP, DUP, PUSH) reports old->new."""
    dbg = deposit_debugger
    for _ in range(40):
        result = dbg.run("si")
        if re.search(r"sp \d+->\d+", "\n".join(result.lines)):
            break
    else:
        raise AssertionError("no stack-height change reported within 40 steps")


def test_info_registers_includes_sizes_and_refund(deposit_debugger):
    result = deposit_debugger.run("info registers")
    joined = "\n".join(result.lines)
    for field in ("calldatasize", "memory", "refund", "sp", "pc"):
        assert field in joined


def test_bare_x_continues_after_last_examination(deposit_debugger):
    """gdb semantics: `x` with no address resumes past the last dump, format reused."""
    dbg = deposit_debugger
    first = dbg.run("x/2xg 0x40")
    assert first.ok and "0x0040" in "\n".join(first.lines)
    second = dbg.run("x/2xg")
    assert "0x0050" in "\n".join(second.lines)
    third = dbg.run("x")
    assert "0x0060" in "\n".join(third.lines)


def test_x_marks_rows_beyond_current_memory(deposit_debugger):
    dbg = deposit_debugger
    result = dbg.run("x/8xg 0x1000")
    joined = "\n".join(result.lines)
    assert "beyond memory" in joined
    assert "reads as zero" in joined


def test_reset_reruns_the_script_and_keeps_breakpoints(deposit_debugger):
    dbg = deposit_debugger
    line = line_of(dbg.session.project, "totalDeposits += amount - fee;")
    assert dbg.run(f"b Bank.sol:{line}").ok
    assert isinstance(dbg.step(StepMode.RUN), Paused)  # consume the first hit
    result = dbg.run("reset")
    assert result.ok and result.lines, "reset must land on a fresh opening stop"
    hit = dbg.run("c")
    assert "Breakpoint 1" in "\n".join(hit.lines), "breakpoint must survive a reset"


def test_run_passes_new_args_to_the_target(deposit_debugger):
    dbg = deposit_debugger
    seen: list[list[str]] = []

    def factory(argv: list[str]):
        seen.append(list(argv))
        return lambda: None  # a target that does nothing finishes immediately

    dbg.session.set_restart_factory(factory, ["original"])
    result = dbg.run("run 0xdeadbeef")
    assert result.ok
    assert seen == [["0xdeadbeef"]], f"factory saw {seen}"


def test_run_expands_at_file_arguments(deposit_debugger, tmp_path):
    """`run @file` reads the argument from a file — payload hex outgrows a console line."""
    dbg = deposit_debugger
    seen: list[list[str]] = []
    payload = tmp_path / "payload.hex"
    payload.write_text("0x" + "00" * 64 + "\n")

    def factory(argv: list[str]):
        seen.append(list(argv))
        return lambda: None

    dbg.session.set_restart_factory(factory, [])
    result = dbg.run(f"run @{payload}")
    assert result.ok
    assert seen == [["0x" + "00" * 64]], seen

    missing = dbg.run(f"run @{tmp_path / 'nope.hex'}")
    assert missing.error and "cannot read" in missing.error
