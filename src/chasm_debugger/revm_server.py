"""Launch the headless REVM JSON-RPC server."""

from __future__ import annotations

from ._revm import serve_stdio


def main() -> int:
    serve_stdio()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
