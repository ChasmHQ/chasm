"""Local HTTP frontend for a live Chasm session."""

from __future__ import annotations

import json
import threading
import webbrowser
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.resources import files
from typing import Any

from rich.text import Text

from .commands import CommandProcessor
from .evaluate import Evaluator
from .session import DebugSession, Finished, Paused
from .tui.layout import storage_rows


class WebFrontend:
    """Expose one paused session to the browser without duplicating command logic."""

    def __init__(
        self,
        session: DebugSession,
        evaluator: Evaluator,
        first_event: Any = None,
        startup_commands: list[str] | None = None,
    ) -> None:
        self.session = session
        self.commands = CommandProcessor(session, evaluator)
        self._function_names = sorted(
            {function.name for function in session.functions.functions if function.name}
        )
        self._lock = threading.RLock()
        self._log = ["(chasm) Web debugger ready."]
        self._closed = threading.Event()
        self._run_startup(list(startup_commands or []))
        if not startup_commands:
            self._record_event(first_event)

    def _run_startup(self, commands: list[str]) -> None:
        for command in commands:
            self._log.append(f"(chasm) {command}")
            self._record_result(self.commands.execute(command))

    def _record_event(self, event: Any) -> None:
        if isinstance(event, Paused):
            self._log.extend(
                _plain(line) for line in self.commands.describe_stop(event.snapshot)
            )
        elif isinstance(event, Finished):
            self._log.append(
                "program finished" if event.ok else f"program raised {event.error}"
            )

    def _record_result(self, result: Any) -> None:
        self._log.extend(_plain(line) for line in result.lines)
        if result.notice:
            self._log.append(result.notice)
        if result.error:
            self._log.append(f"error: {result.error}")
        if result.quit:
            self._closed.set()

    def command(self, line: str) -> dict[str, Any]:
        with self._lock:
            self._log.append(f"(chasm) {line}")
            self._record_result(self.commands.execute(line))
            return self.state()

    def state(self) -> dict[str, Any]:
        with self._lock:
            snap = self.session.last_snapshot
            if snap is None or self.session.finished:
                return {
                    "report": {
                        "finished": self.session.finished,
                        "stopped": False,
                    },
                    "error": self.session.exit_error,
                    "functions": self._function_names,
                    "log": self._log[-500:],
                }
            return self._snapshot(snap)

    def _snapshot(self, snap: Any) -> dict[str, Any]:
        source = (
            self.session.project.sources.get(snap.source_key) if snap.source_key else None
        )
        source_rows = []
        if source is not None:
            source_rows = [
                {"line": index, "text": text, "current": index == snap.line}
                for index, text in enumerate(source.text.splitlines(), 1)
            ]
        try:
            disassembly = self.session.inspect("disassembly", 6, 18)
        except Exception:
            disassembly = []
        try:
            decoder = self.commands.decoder(snap.contract_name)
            rows = storage_rows(
                decoder, lambda slot: self.session.inspect("read_storage", slot)
            )
            storage = [
                {
                    "slot": slot,
                    "name": f"{name} ({type_name})",
                    "value": value,
                }
                for slot, _offset, name, type_name, value in rows
            ]
        except Exception:
            storage = []
        locals_ = [
            {
                "name": value.name,
                "type": value.type_label,
                "value": value.display if value.available else "<unavailable>",
            }
            for value in snap.locals
        ]
        return {
            "report": {
                "stopped": True,
                "finished": False,
                "pc": snap.pc,
                "gas": {"remaining": snap.gas_remaining},
                "depth": snap.depth,
                "mnemonic": snap.mnemonic,
                "step": snap.step,
                "location": {
                    "file": snap.source_key,
                    "line": snap.line,
                    "function": snap.function.signature
                    if snap.function
                    else snap.contract_name,
                },
            },
            "source": {"file": snap.source_key, "items": source_rows},
            "disassembly": {"rows": disassembly},
            "stack": {
                "items": [
                    {"index": item.index, "hex": item.hex(), "decimal": item.value}
                    for item in snap.stack
                ]
            },
            "backtrace": {
                "frames": [
                    {
                        "index": row.index,
                        "name": row.name,
                        "file": row.source_key,
                        "line": row.line,
                        "pc": row.pc,
                    }
                    for row in snap.backtrace
                ]
            },
            "locals": {"locals": locals_},
            "storage": {"items": storage},
            "functions": self._function_names,
            "log": self._log[-500:],
        }

    def close(self) -> None:
        self._closed.set()
        self.session.detach(timeout=5.0)


def serve(
    frontend: WebFrontend,
    host: str = "127.0.0.1",
    port: int = 8765,
    open_browser: bool = True,
) -> None:
    """Serve the local browser UI until its process receives Ctrl+C."""

    handler = _handler(frontend)
    server = ThreadingHTTPServer((host, port), handler)
    url = f"http://{host}:{server.server_port}"
    print(f"Chasm web UI: {url}")
    print("Press Ctrl+C to stop the server and detach the debugger.")
    if open_browser:
        webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        frontend.close()


def _handler(frontend: WebFrontend) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802
            if self.path in {"/", "/index.html"}:
                self._send(HTTPStatus.OK, "text/html; charset=utf-8", _html())
            elif self.path == "/api/state":
                self._json(HTTPStatus.OK, frontend.state())
            else:
                self._send(
                    HTTPStatus.NOT_FOUND, "text/plain; charset=utf-8", b"not found"
                )

        def do_POST(self) -> None:  # noqa: N802
            if self.path != "/api/command":
                self._send(
                    HTTPStatus.NOT_FOUND, "text/plain; charset=utf-8", b"not found"
                )
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(length))
                line = payload["command"]
                if not isinstance(line, str) or not line.strip():
                    raise ValueError("command must be a non-empty string")
            except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
                self._json(HTTPStatus.BAD_REQUEST, {"error": str(exc), "log": []})
                return
            self._json(HTTPStatus.OK, frontend.command(line.strip()))

        def log_message(self, _format: str, *_args: Any) -> None:
            return

        def _json(self, status: HTTPStatus, payload: dict[str, Any]) -> None:
            self._send(
                status,
                "application/json; charset=utf-8",
                json.dumps(payload).encode(),
            )

        def _send(self, status: HTTPStatus, content_type: str, body: bytes) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

    return Handler


def _html() -> bytes:
    return files("chasm_debugger").joinpath("web.html").read_bytes()


def _plain(markup: str) -> str:
    try:
        return Text.from_markup(markup).plain
    except Exception:
        return markup
