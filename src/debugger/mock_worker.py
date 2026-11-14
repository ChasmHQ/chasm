"""Local JSONL fixture: no network, native engine, or Foundry required."""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

record = Path(sys.argv[0]).with_suffix(".record")
for line in sys.stdin:
    request = json.loads(line)
    with record.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(request) + "\n")
    method = request["method"]
    params = request.get("params", {})
    if method == "hello":
        result = {"protocol": "chasm-web-debugger/1", "available": True}
    elif method == "start":
        result = {"status": {"state": "paused"}, "pc": 0, "params": params}
    elif params.get("action") == "hang":
        # Verify independent cancellation kills a descendant, not just the service.
        descendant = subprocess.Popen([sys.executable, "-I", "-c", "import time; time.sleep(120)"])
        record.with_suffix(".child").write_text(str(descendant.pid), encoding="ascii")
        time.sleep(120)
        result = {}
    elif params.get("action") == "crash":
        os._exit(23)
    elif params.get("action") == "oversized":
        sys.stdout.write("x" * (16 * 1024 * 1024 + 1))
        sys.stdout.flush()
        time.sleep(120)
    elif params.get("action") == "error":
        print(json.dumps({"id": request["id"], "error": {"code": "operation_failed", "message": "mock rejected https://secret.example/token"}}), flush=True)
        continue
    elif method == "evaluate":
        result = {"evaluation": {"value": "42"}, "snapshot": {"status": {"state": "paused"}, "pc": 1}}
    elif method == "export_capture":
        result = {"capture": "mock", "steps": []}
    else:
        result = {"status": {"state": "finished" if params.get("action") == "finish" else "paused"}, "pc": 1}
    print(json.dumps({"id": request["id"], "result": result}), flush=True)
