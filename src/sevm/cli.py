"""`sevm run script.py` - the entry point.

By decision, the debugger attaches to the user's own script rather than owning the
setup. The script drives web3 exactly as it already does; sevm compiles the contracts,
patches Py-EVM process-wide, runs the script on the VM thread, and stops the first time
execution enters code it recognises.

Recognition is by bytecode, not by configuration: every contract under --contracts is
compiled, and a deployed account's runtime code is matched against those artifacts with
the metadata hash stripped and immutables masked. So an unmodified script works.
"""

from __future__ import annotations

import argparse
import os
import runpy
import sys
from collections.abc import Sequence
from typing import Any

from .commands.parsing import expand_file_args
from .compile import CompileError, compile_foundry_project, find_foundry_root, solcbin
from .evaluate import Evaluator, make_eval_hook
from .session import DebugSession, Finished, StepMode


def _find_contracts_dir(script_path: str, explicit: str | None) -> str:
    if explicit:
        return explicit
    here = os.path.dirname(os.path.abspath(script_path)) or os.getcwd()
    for candidate in ("contracts", "src", "."):
        path = os.path.join(here, candidate)
        if os.path.isdir(path) and any(f.endswith(".sol") for f in os.listdir(path)):
            return path
    return os.path.join(here, "contracts")


def _run_script(path: str, argv: Sequence[str]):
    """Execute the user's script as __main__, with its own directory importable."""
    directory = os.path.dirname(os.path.abspath(path))
    if directory not in sys.path:
        sys.path.insert(0, directory)

    def target() -> None:
        saved = sys.argv[:]
        sys.argv = [path, *argv]
        try:
            runpy.run_path(path, run_name="__main__")
        finally:
            sys.argv = saved

    return target


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="sevm",
        description="A gdb-style interactive Solidity/EVM debugger running on Py-EVM.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    run = sub.add_parser(
        "run",
        help="debug a Python script that drives web3",
        usage="sevm run [options] SCRIPT [script args...]",
        epilog="Options must come BEFORE the script; everything after it is passed to the script.",
    )
    run.add_argument(
        "script",
        help="a web3 .py driver script, or a Foundry .sol test to debug",
    )
    run.add_argument(
        "args", nargs=argparse.REMAINDER, help="arguments passed to the script"
    )
    run.add_argument(
        "-c", "--contracts", help="directory of .sol sources (default: ./contracts)"
    )
    run.add_argument("--solc", default=None, help="solc version to compile with")
    run.add_argument(
        "--solc-binary",
        default=None,
        metavar="PATH",
        help="use this solc executable instead of downloading one (env: SEVM_SOLC)",
    )
    run.add_argument(
        "--console", action="store_true", help="plain text frontend instead of the TUI"
    )
    run.add_argument(
        "--optimize",
        action="store_true",
        help="compile with the optimizer on (degrades source maps; not recommended)",
    )
    run.add_argument(
        "--no-mouse",
        action="store_true",
        help="disable mouse reporting, handing text selection back to the terminal",
    )
    run.add_argument(
        "-x",
        "--exec",
        action="append",
        default=[],
        metavar="CMD",
        help="run a debugger command at startup; repeatable",
    )
    run.add_argument(
        "--timeout", type=float, default=120.0, help="seconds to wait for the first stop"
    )
    run.add_argument(
        "-m",
        "--match",
        default=None,
        help="(.sol tests) substring of the test function to debug",
    )
    run.add_argument(
        "--match-contract",
        default=None,
        help="(.sol tests) substring of the test contract to debug",
    )
    run.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="assume yes: write foundry.toml and install missing libraries without prompting",
    )
    run.add_argument(
        "--no-install",
        action="store_true",
        help="never fetch a library; fail if an import is not already on disk",
    )
    run.add_argument(
        "--no-cache",
        action="store_true",
        help="write nothing: no build cache, no out/ artifacts",
    )
    run.add_argument(
        "--force", action="store_true", help="recompile even if the cache has this build"
    )

    compile_cmd = sub.add_parser(
        "compile", help="compile contracts and report what sevm sees"
    )
    compile_cmd.add_argument("contracts", help="directory or file of .sol sources")
    compile_cmd.add_argument("--solc", default=None)
    compile_cmd.add_argument("--solc-binary", default=None, metavar="PATH")
    compile_cmd.add_argument(
        "-y",
        "--yes",
        action="store_true",
        help="assume yes: write foundry.toml and install missing libraries without prompting",
    )
    compile_cmd.add_argument(
        "--no-install",
        action="store_true",
        help="never fetch a library; fail if an import is not already on disk",
    )
    compile_cmd.add_argument(
        "--no-cache",
        action="store_true",
        help="write nothing: no build cache, no out/ artifacts",
    )
    compile_cmd.add_argument(
        "--force", action="store_true", help="recompile even if the cache has this build"
    )

    sub.add_parser(
        "doctor", help="report the compiler, runtimes and caches sevm found here"
    )

    return parser


def cmd_doctor(_args: argparse.Namespace) -> int:
    """Print what sevm found, and fail if none of it can compile."""
    from . import doctor

    lines = doctor.report()
    print(doctor.render(lines))
    return 0 if doctor.ok(lines) else 1


def cmd_compile(args: argparse.Namespace) -> int:
    from rich.console import Console

    from .foundry import prepare_project

    console = Console()
    target = os.path.abspath(args.contracts)
    if not os.path.exists(target):
        console.print(f"[bold red]no such path:[/bold red] {args.contracts}")
        return 1

    base = target if os.path.isdir(target) else os.path.dirname(target)
    prepared = prepare_project(
        base,
        assume_yes=args.yes,
        allow_install=not args.no_install,
        needs_forge_std=False,
    )
    root = prepared.root
    source_dirs = [base] if os.path.isdir(target) and base != root else None
    try:
        project = compile_foundry_project(
            root,
            target_file=None if os.path.isdir(target) else target,
            source_dirs=source_dirs,
            solc_version=args.solc,
            install_missing=prepared.may_install,
            on_notice=lambda msg: console.print(f"[dim]{msg}[/dim]", highlight=False),
            use_cache=not args.no_cache,
            force=args.force,
        )
    except CompileError as exc:
        console.print(f"[bold red]compile failed:[/bold red] {exc}", highlight=False)
        return 1
    console.print(f"[green]solc {project.solc_version}[/green], optimizer off")
    for key, src in project.sources.items():
        console.print(
            f"  [cyan]{key}[/cyan] [dim]id={src.file_id} lines={len(src.text.splitlines())}[/dim]"
        )
    for name, art in project.artifacts.items():
        has_map = "yes" if art.deployed_source_map else "[red]NO[/red]"
        layout = len((art.storage_layout or {}).get("storage", []))
        console.print(
            f"  [bold]{name}[/bold] runtime={len(art.deployed_bytecode)}B "
            f"source-map={has_map} state-vars={layout} fns={len(art.method_identifiers)}"
        )
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    from rich.console import Console

    console = Console()
    script = args.script
    if not os.path.isfile(script):
        console.print(f"[bold red]no such script:[/bold red] {script}")
        return 1

    # Everything after the script is forwarded to it verbatim, so a sevm option placed
    # there silently does nothing; name that failure instead of leaving it confusing.
    stray = [
        a
        for a in args.args
        if a.startswith("--")
        and a.lstrip("-").split("=")[0]
        in {
            "console",
            "contracts",
            "solc",
            "optimize",
            "exec",
            "timeout",
            "match",
            "match-contract",
            "yes",
            "no-install",
            "no-cache",
            "force",
        }
    ]
    if stray:
        console.print(
            f"[yellow]note:[/yellow] {' '.join(stray)} came after the script, so "
            "it was passed to the script rather than to sevm. Options go before it:\n"
            f"  sevm run {' '.join(stray)} {script}"
        )

    # Dispatch by extension: a Foundry Solidity test, or the classic web3 Python driver.
    if script.endswith(".sol"):
        return _run_foundry(console, args)
    return _run_python(console, args)


def _warn_if_declined(console: Any, prepared: Any) -> None:
    """Say why nothing was installed. Without a terminal the prompt cannot even be shown."""
    if not prepared.declined or not prepared.missing:
        return
    console.print(
        f"[yellow]note:[/yellow] not installing {', '.join(prepared.missing)}. "
        "Pass -y to allow it.",
        highlight=False,
    )


def _run_python(console: Any, args: argparse.Namespace) -> int:
    from .foundry import prepare_project

    script = args.script
    contracts = _find_contracts_dir(script, args.contracts)
    if not os.path.isdir(contracts):
        console.print(
            f"[bold red]no contracts directory[/bold red] at {contracts}. "
            "Pass one with --contracts."
        )
        return 1

    # The contracts keep their own directory as the compile root unless they sit inside a
    # Foundry project, whose foundry.toml/remappings/lib then apply as they do to a test.
    contracts = os.path.abspath(contracts)
    root = find_foundry_root(contracts) or contracts
    source_dirs = [contracts] if os.path.abspath(root) != contracts else None
    prepared = prepare_project(
        contracts,
        assume_yes=args.yes,
        allow_install=not args.no_install,
        # A web3 driver only needs forge-std if its contracts import it, and then the
        # import scan installs it; do not fetch it for contracts that never mention it.
        needs_forge_std=False,
        source_dirs=source_dirs,
    )

    console.print(f"[dim]compiling {contracts} ...[/dim]", highlight=False)
    _warn_if_declined(console, prepared)
    try:
        project = compile_foundry_project(
            prepared.root,
            source_dirs=source_dirs,
            solc_version=args.solc,
            optimize=args.optimize,
            install_missing=prepared.may_install,
            on_notice=lambda msg: console.print(f"[dim]{msg}[/dim]", highlight=False),
            use_cache=not args.no_cache,
            force=args.force,
        )
    except CompileError as exc:
        console.print(f"[bold red]compile failed:[/bold red] {exc}", highlight=False)
        return 1
    if args.optimize:
        console.print(
            "[yellow]warning:[/yellow] the optimizer is on. Source maps and stepping will "
            "be less accurate."
        )
    console.print(
        f"[dim]{len(project.artifacts)} contract(s): "
        f"{', '.join(a.name for a in project.artifacts.values())}[/dim]",
        highlight=False,
    )
    # `@path` script arguments are read from that file: payload hex outgrows what a
    # Windows command line (or console input) will carry. Same syntax as `run @file`.
    expanded = expand_file_args(list(args.args), " ".join(args.args))
    if isinstance(expanded, str):
        console.print(f"[bold red]{expanded}[/bold red]", highlight=False)
        return 1
    target = _run_script(script, expanded)
    return _debug(
        console,
        project,
        target,
        args,
        foundry_mode=True,
        restart_factory=lambda argv: _run_script(script, argv),
        restart_argv=expanded,
    )


def _run_foundry(console: Any, args: argparse.Namespace) -> int:
    from .foundry import (
        compile_test,
        discover_tests,
        make_tests_driver,
        prepare_project,
        select_tests,
    )

    sol = args.script
    prepared = prepare_project(
        sol, assume_yes=args.yes, allow_install=not args.no_install
    )
    kind = "foundry project" if prepared.existing else "standalone test"
    console.print(f"[dim]{kind} at {prepared.root}; compiling ...[/dim]", highlight=False)
    _warn_if_declined(console, prepared)
    try:
        project = compile_test(
            sol,
            prepared.root,
            solc_version=args.solc,
            install_missing=prepared.may_install,
            on_notice=lambda msg: console.print(f"[dim]{msg}[/dim]", highlight=False),
            use_cache=not args.no_cache,
            force=args.force,
        )
    except CompileError as exc:
        console.print(f"[bold red]compile failed:[/bold red] {exc}", highlight=False)
        return 1

    targets = discover_tests(project)
    if not targets:
        console.print(
            "[bold red]no test functions found[/bold red] "
            "(looked for no-argument test*/invariant* functions)"
        )
        return 1
    # No filter debugs every test in turn; -m/--match-contract narrows the set.
    selected = select_tests(targets, match=args.match, match_contract=args.match_contract)
    if not selected:
        console.print(
            f"[bold red]no test matched[/bold red] "
            f"match={args.match!r} contract={args.match_contract!r}. Available: "
            + ", ".join(f"{t.contract}.{t.function}" for t in targets)
        )
        return 1
    names = ", ".join(f"{t.contract}.{t.function}" for t in selected)
    console.print(
        f"[dim]debugging {len(selected)} test(s): {names}[/dim]", highlight=False
    )
    driver = make_tests_driver(project, selected)
    return _debug(
        console,
        project,
        driver,
        args,
        foundry_mode=True,
        stop_functions=[f"{t.contract}.{t.function}" for t in selected],
    )


def _debug(
    console: Any,
    project: Any,
    target: Any,
    args: argparse.Namespace,
    foundry_mode: bool,
    stop_functions: list[str] | None = None,
    restart_factory: Any = None,
    restart_argv: list[str] | None = None,
) -> int:
    """Shared tail: start the target on the VM thread and hand off to a frontend.

    With `stop_functions`, a breakpoint is set on each (every test body). The session runs
    past deployment and `setUp` and opens at the first one; `continue` then stops at each
    subsequent test in turn. `restart_factory` binds the `reset` / `run` commands.
    """
    session = DebugSession(project)
    session.foundry_mode = foundry_mode
    evaluator = Evaluator(project)
    session.set_eval_hook(make_eval_hook(evaluator))
    if restart_factory is not None:
        session.set_restart_factory(restart_factory, restart_argv or [])

    startup_commands = list(args.exec)
    first_contract = first_fn = None
    for name in stop_functions or []:
        try:
            session.break_at_function(name)
        except Exception:
            continue
        if first_contract is None:
            first_contract, _, first_fn = name.partition(".")

    session.start(target)
    first = session.wait(timeout=args.timeout)

    if first is None:
        console.print(
            "[bold red]timed out waiting for the script to reach contract code[/bold red]"
        )
        session.detach()
        return 1

    # Advance from the unavoidable first stop (the constructor) to the first test body.
    # Breakpoints on the other tests stay set so `continue` stops at each in turn.
    if first_fn is not None:
        try:
            for _ in range(64):
                snap = session.last_snapshot
                fn = getattr(snap, "function", None)
                if (
                    fn is not None
                    and getattr(fn, "name", None) == first_fn
                    and getattr(fn, "contract", None) == first_contract
                ):
                    break
                advanced = session.resume(StepMode.RUN, count=1, timeout=args.timeout)
                if advanced is not None:
                    first = advanced
                if isinstance(advanced, Finished):
                    break
        except Exception:
            pass

    if args.console:
        from .console import ConsoleFrontend

        frontend = ConsoleFrontend(session, evaluator)
        for command in startup_commands:
            frontend._emit(frontend.commands.execute(command))
        frontend.run(first_event=first)
        return 0

    try:
        from .tui.app import SevmApp
    except ImportError as exc:
        console.print(
            f"[yellow]the TUI needs Textual ({exc}). Install it with "
            "`pip install textual`, or use --console.[/yellow]"
        )
        from .console import ConsoleFrontend

        ConsoleFrontend(session, evaluator).run(first_event=first)
        return 0

    app = SevmApp(
        session, evaluator, first_event=first, startup_commands=startup_commands
    )
    # Mouse on: every pane renders Textual `Content`, which the framework can
    # select/highlight/copy (drag to select, ctrl+c to copy). `--no-mouse` hands
    # selection back to the terminal.
    app.run(mouse=not args.no_mouse)
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if getattr(args, "solc_binary", None):
        solcbin.use_binary(args.solc_binary)
    if args.command == "run":
        return cmd_run(args)
    if args.command == "compile":
        return cmd_compile(args)
    if args.command == "doctor":
        return cmd_doctor(args)
    parser.print_help()
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
