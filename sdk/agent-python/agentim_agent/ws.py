"""A minimal WebSocket client (RFC 6455) on asyncio, standard library only:
text frames, ping/pong, close. Enough for the AgentIM protocol."""

import asyncio
import base64
import os
import ssl
import struct
from urllib.parse import urlsplit


class ConnectionClosed(Exception):
    pass


class HandshakeError(Exception):
    def __init__(self, status):
        super().__init__(f"handshake refused: HTTP {status}")
        self.status = status


class WebSocket:
    def __init__(self, reader, writer):
        self._r = reader
        self._w = writer
        self._lock = asyncio.Lock()
        self.closed = False

    @classmethod
    async def connect(cls, url, timeout=10):
        u = urlsplit(url)
        secure = u.scheme == "wss"
        port = u.port or (443 if secure else 80)
        ctx = ssl.create_default_context() if secure else None
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(u.hostname, port, ssl=ctx), timeout
        )
        key = base64.b64encode(os.urandom(16)).decode()
        path = (u.path or "/") + (f"?{u.query}" if u.query else "")
        host = u.hostname if u.port is None else f"{u.hostname}:{u.port}"
        writer.write(
            (
                f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        await writer.drain()
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout)
        status = int(head.split(b" ", 2)[1])
        if status != 101:
            writer.close()
            raise HandshakeError(status)
        return cls(reader, writer)

    async def send(self, text):
        data = text.encode()
        header = bytearray([0x81])  # FIN, text
        n = len(data)
        if n < 126:
            header.append(0x80 | n)
        elif n < 1 << 16:
            header.append(0x80 | 126)
            header += struct.pack(">H", n)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", n)
        await self._frame(bytes(header), data)

    async def _frame(self, header, data):
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        async with self._lock:
            if self.closed:
                raise ConnectionClosed("closed")
            self._w.write(header + mask + masked)
            await self._w.drain()

    async def _control(self, opcode, data=b""):
        await self._frame(bytes([0x80 | opcode, 0x80 | len(data)]), data)

    async def recv(self):
        """The next text message; raises ConnectionClosed."""
        parts = []
        while True:
            try:
                b1, b2 = await self._r.readexactly(2)
                n = b2 & 0x7F
                if n == 126:
                    (n,) = struct.unpack(">H", await self._r.readexactly(2))
                elif n == 127:
                    (n,) = struct.unpack(">Q", await self._r.readexactly(8))
                mask = await self._r.readexactly(4) if b2 & 0x80 else None
                data = await self._r.readexactly(n)
            except (asyncio.IncompleteReadError, ConnectionError, OSError) as e:
                self.closed = True
                raise ConnectionClosed(str(e)) from e
            if mask:
                data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
            op = b1 & 0x0F
            if op == 0x8:
                self.closed = True
                raise ConnectionClosed(data[2:].decode(errors="replace"))
            if op == 0x9:
                await self._control(0xA, data)
                continue
            if op == 0xA:
                continue
            parts.append(data)
            if b1 & 0x80:
                return b"".join(parts).decode()

    async def close(self):
        if not self.closed:
            try:
                await self._control(0x8, struct.pack(">H", 1000))
            except Exception:
                pass
        self.closed = True
        self._w.close()
