"""Run the live flows against a fresh Chasm, in order, and print a summary.

Usage:
  python tests/live/run.py                  # builds nothing; uses target/debug/chasm.exe
  python tests/live/run.py bank revert      # only these flows (bank always runs first)
  CHASM_SKIP_FORK=1 python tests/live/run.py   # no network access

Starts Chasm on a temporary copy of contracts/ with --control-token-file, so
the flows never touch your real .chasm/ folder. Needs a debug build, Foundry
in PATH, and port 8545 free.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
EXE = os.path.join(ROOT, "target", "debug", "chasm.exe" if os.name == "nt" else "chasm")


def main():
    if not os.path.exists(EXE):
        raise SystemExit(f"{EXE} not found; run cargo build first.")
    work = tempfile.mkdtemp(prefix="chasm-live-")
    workspace = os.path.join(work, "contracts")
    shutil.copytree(os.path.join(ROOT, "contracts"), workspace, ignore=shutil.ignore_patterns(".chasm", "out", "cache", "*.json"))
    token_file = os.path.join(work, "token.txt")
    os.environ["CHASM_TOKEN_FILE"] = token_file
    os.environ["CHASM_WORKSPACE"] = workspace
    sys.path.insert(0, HERE)
    import flows
    wanted = sys.argv[1:]
    chosen = [flow for flow in flows.FLOWS if not wanted or flow.__name__ in wanted or flow.__name__ == "bank"]
    process = subprocess.Popen([EXE, workspace, "--control-token-file", token_file], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    results = []
    try:
        for _ in range(90):
            if os.path.exists(token_file) and os.path.getsize(token_file) > 0:
                break
            time.sleep(1)
        else:
            raise SystemExit("Chasm did not write the token file.")
        time.sleep(8)  # first compile
        for flow in chosen:
            print(f"\n== {flow.__name__}: {(flow.__doc__ or '').strip().splitlines()[0]}")
            started = time.time()
            try:
                flow()
                results.append((flow.__name__, "pass", time.time() - started))
            except SystemExit as error:
                results.append((flow.__name__, "FAIL", time.time() - started))
                print(f"  FAIL {error.code!r}"[:600], flush=True)
                if flow.__name__ == "bank":
                    break
            except Exception:
                traceback.print_exc()
                sys.stderr.flush()
                results.append((flow.__name__, "FAIL", time.time() - started))
    finally:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
        if os.name == "nt":
            subprocess.run(["taskkill", "/F", "/IM", "anvil.exe"], capture_output=True)
        shutil.rmtree(work, ignore_errors=True)
    print("\nflow           result  seconds")
    for name, result, seconds in results:
        print(f"{name:<14} {result:<7} {seconds:6.1f}")
    failed = [name for name, result, _ in results if result != "pass"]
    print(f"\n{len(results) - len(failed)} passed, {len(failed)} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
