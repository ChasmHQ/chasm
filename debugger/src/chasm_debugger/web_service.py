"""JSONL worker entry point; stdout is reserved for protocol responses."""

from __future__ import annotations

import io
import json
import os
import sys
from contextlib import suppress
from typing import Any, BinaryIO, TextIO

from .web.protocol import (
    MAX_REQUEST_BYTES,
    MAX_RESPONSE_BYTES,
    WebError,
    reject_constant,
    unique_object,
)


def _protocol_stdout() -> TextIO:
    sys.stdout.flush()
    # A duplicate survives dup2 and keeps compiler/native/user output off the wire.
    descriptor = os.dup(sys.stdout.fileno())
    os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
    sys.stdout = sys.stderr
    sys.__dict__["__stdout__"] = sys.stderr
    return io.TextIOWrapper(
        os.fdopen(descriptor, "wb"), encoding="utf-8", newline="\n", write_through=True
    )


def serve(input_stream: BinaryIO, output: TextIO) -> int:
    from .web.service import WebService

    service = WebService()
    try:
        while not service.closed:
            line = input_stream.readline(MAX_REQUEST_BYTES + 1)
            if not line:
                break
            request_id = ""
            response: dict[str, Any]
            try:
                if len(line) > MAX_REQUEST_BYTES:
                    while line and not line.endswith(b"\n"):
                        line = input_stream.readline(MAX_REQUEST_BYTES + 1)
                    raise WebError(
                        "invalid_request", "request exceeds maximum JSONL byte length"
                    )
                request = json.loads(
                    line, object_pairs_hook=unique_object, parse_constant=reject_constant
                )
                if (
                    isinstance(request, dict)
                    and isinstance(request.get("id"), str)
                    and len(request["id"]) <= 256
                ):
                    request_id = request["id"]
                result = service.dispatch(request)
                response = {"id": request_id, "result": result}
            except WebError as exc:
                response = {
                    "id": request_id,
                    "error": {"code": exc.code, "message": service.message(exc)},
                }
            except (ValueError, UnicodeError, RecursionError):
                response = {
                    "id": request_id,
                    "error": {
                        "code": "invalid_request",
                        "message": "invalid JSON request",
                    },
                }
            except Exception as exc:
                response = {
                    "id": request_id,
                    "error": {
                        "code": "operation_failed",
                        "message": service.message(exc),
                    },
                }
            encoded = json.dumps(
                response, separators=(",", ":"), ensure_ascii=True, allow_nan=False
            )
            if len(encoded) + 1 > MAX_RESPONSE_BYTES:
                encoded = json.dumps(
                    {
                        "id": request_id,
                        "error": {
                            "code": "response_too_large",
                            "message": "response exceeds maximum JSONL byte length",
                        },
                    }
                )
            output.write(encoded + "\n")
            output.flush()
    finally:
        with suppress(Exception):
            service.driver.stop()
    return 0


def main() -> int:
    output = _protocol_stdout()
    try:
        return serve(sys.stdin.buffer, output)
    except (BrokenPipeError, KeyboardInterrupt):
        return 0
    finally:
        with suppress(OSError):
            output.close()


if __name__ == "__main__":
    raise SystemExit(main())
