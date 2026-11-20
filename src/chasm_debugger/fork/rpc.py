from __future__ import annotations

from typing import Any
from urllib.parse import urlsplit

import requests

from .validation import ForkError, obj

_METHODS = frozenset(
    {
        "eth_chainId",
        "eth_getBlockByNumber",
        "eth_getBlockByHash",
        "eth_getTransactionByHash",
        "eth_getTransactionReceipt",
    }
)


class MetadataRpc:
    def __init__(self, url: str) -> None:
        try:
            parsed = urlsplit(url)
            valid = (
                parsed.scheme in {"http", "https"}
                and parsed.hostname
                and not parsed.fragment
            )
            _port = parsed.port
        except (TypeError, ValueError):
            valid = False
        if not valid:
            raise ForkError("RPC URL must be an explicit HTTP(S) endpoint")
        self._url = url
        self._session = requests.Session()
        self._session.trust_env = False
        self._counter = 0

    def __enter__(self) -> MetadataRpc:
        return self

    def __exit__(self, *_: Any) -> None:
        self._session.close()

    def call(self, method: str, params: list[Any]) -> Any:
        if method not in _METHODS:
            raise ForkError("RPC method is not permitted by the metadata reader")
        self._counter += 1
        try:
            response = self._session.post(
                self._url,
                json={
                    "jsonrpc": "2.0",
                    "id": self._counter,
                    "method": method,
                    "params": params,
                },
                timeout=(5, 30),
                allow_redirects=False,
            )
            if response.status_code != 200:
                raise ForkError(
                    f"metadata RPC {method} returned HTTP {response.status_code}"
                )
            payload = obj(response.json(), "RPC response")
        except (requests.RequestException, ValueError):
            # Provider messages and requests exceptions can contain credential-bearing URLs.
            raise ForkError(
                f"metadata RPC {method} failed (endpoint details redacted)"
            ) from None
        if (
            payload.get("jsonrpc") != "2.0"
            or type(payload.get("id")) is not int
            or payload["id"] != self._counter
        ):
            raise ForkError("invalid metadata RPC response identity")
        if "error" in payload or "result" not in payload:
            raise ForkError(f"metadata RPC {method} failed (provider details redacted)")
        if payload["result"] is None:
            raise ForkError(f"metadata RPC {method} returned no result")
        return payload["result"]
