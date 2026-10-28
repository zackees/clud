"""MCP-owned lifecycle for one local, authenticated model gateway."""

from __future__ import annotations

import asyncio
import os
import secrets
import signal
from urllib.parse import urlsplit

import anyio

_STARTUP_TIMEOUT = 30
_STOP_TIMEOUT = 10


class GatewayManager:
    """Keep a gateway reachable by handle and reap it on MCP shutdown."""

    def __init__(self) -> None:
        self._lock = asyncio.Lock()
        self._process: asyncio.subprocess.Process | None = None
        self._token = ""
        self._url = ""
        self._selection: tuple[str, bool, int] | None = None
        self._drains: list[asyncio.Task] = []

    def status(self) -> dict:
        running = self._process is not None and self._process.returncode is None
        if not running:
            return {"running": False}
        if not self._url:
            return {"running": False, "starting": True}
        return {
            "running": True,
            "base_url": self._url,
            "protocol": "anthropic_messages",
            "models_url": f"{self._url}/v1/models",
            "messages_url": f"{self._url}/v1/messages",
            "auth_header": "x-api-key",
            "client_token": self._token,
            "backend": self._selection[0],
            "openrouter": self._selection[1],
            "lifetime": "MCP server connection; stopped on disconnect",
        }

    async def start(self, binary: str, backend: str, openrouter: bool, port: int) -> dict:
        if backend not in ("claude", "codex"):
            return {"error": "backend must be claude or codex"}
        if not 0 <= port <= 65535:
            return {"error": "port must be between 0 and 65535"}
        async with self._lock:
            selection = (backend, openrouter, port)
            if self.status()["running"]:
                if selection != self._selection:
                    return {"error": "stop the existing gateway before changing its configuration"}
                return self.status()
            await self._stop()
            token = secrets.token_urlsafe(32)
            env = {**os.environ, "CLUD_SERVE_CLIENT_TOKEN": token}
            argv = [binary, "serve", f"--{backend}", "--port", str(port)]
            if openrouter:
                argv.append("--openrouter")
            try:
                self._process = await asyncio.create_subprocess_exec(
                    *argv,
                    env=env,
                    stdin=asyncio.subprocess.DEVNULL,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                )
                self._drains = [asyncio.create_task(self._discard(self._process.stderr))]
                line = await asyncio.wait_for(
                    self._process.stdout.readline(), timeout=_STARTUP_TIMEOUT
                )
                prefix = "clud serve listening at "
                banner = line.decode("utf-8", "replace").strip()
                if not banner.startswith(prefix):
                    raise ValueError("missing startup banner")
                url = banner.removeprefix(prefix)
                address = urlsplit(url)
                if (
                    address.scheme != "http"
                    or address.hostname != "127.0.0.1"
                    or address.username is not None
                    or address.password is not None
                    or not address.port
                    or address.path not in ("", "/")
                    or address.query
                    or address.fragment
                ):
                    raise ValueError("invalid gateway endpoint")
                self._token, self._url, self._selection = token, url, selection
                self._drains.append(asyncio.create_task(self._discard(self._process.stdout)))
                return self.status()
            except asyncio.CancelledError:
                with anyio.CancelScope(shield=True):
                    await self._stop()
                raise
            except (OSError, ValueError, asyncio.TimeoutError):
                await self._stop()
                return {
                    "error": "gateway startup failed or timed out",
                    "remedy": (
                        "Check the port and upstream credentials: clud auth login codex "
                        "--acknowledge-experimental or OPENAI_API_KEY for Codex; "
                        "ANTHROPIC_API_KEY for Claude; clud auth login openrouter for OpenRouter"
                    ),
                }

    @staticmethod
    async def _discard(stream: asyncio.StreamReader) -> None:
        # Drain without retaining/logging provider output or secret-bearing diagnostics.
        while await stream.read(8192):
            pass

    async def stop(self) -> dict:
        async with self._lock:
            with anyio.CancelScope(shield=True):
                await self._stop()
            return {"running": False}

    async def _stop(self) -> None:
        process = self._process
        try:
            if process is not None and process.returncode is None:
                try:
                    if os.name == "nt":
                        process.terminate()
                    else:
                        process.send_signal(signal.SIGINT)
                except ProcessLookupError:
                    pass
                try:
                    await asyncio.wait_for(process.wait(), timeout=_STOP_TIMEOUT)
                except asyncio.TimeoutError:
                    try:
                        process.kill()
                    except ProcessLookupError:
                        pass
                    await asyncio.wait_for(process.wait(), timeout=5)
        finally:
            for task in self._drains:
                task.cancel()
            await asyncio.gather(*self._drains, return_exceptions=True)
            self._drains = []
            self._process = None
            self._url, self._token, self._selection = "", "", None
