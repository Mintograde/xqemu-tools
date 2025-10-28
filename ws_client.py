import asyncio
import base64
import json
from queue import Queue, Empty
from urllib.parse import quote

import websockets
import zstandard as zstd
import orjson

from typing import Unpack, TypedDict


class ClientBaseKwargs(TypedDict, total=False):
    preempt_key: str
    always_include_key: bool


class SendKwargs(TypedDict, total=False):
    buffer_messages: bool
    compress_messages: bool
    max_buffer_size: int


class ClientKwargs(ClientBaseKwargs, SendKwargs, total=False):
    pass


async def recv_loop(ws, state):
    """
    Handle incoming messages from the websocket server.
    """
    print("[ws_client] Receiver loop started.")
    try:
        async for raw in ws:
            try:
                msg = json.loads(raw) if isinstance(raw, (str, bytes, bytearray)) else raw
            except json.JSONDecodeError:
                print(f"[ws_client][recv] non-json: {raw!r}")
                continue

            print(f"[ws_client][recv] {msg}")

            if isinstance(msg, dict) and msg.get("type") == "error" and msg.get("code") == "BAD_KEY":
                state["require_key"] = True
                print("[ws_client][info] Server requires per-message key. Will include it in subsequent messages.")
    except websockets.ConnectionClosed as e:
        print(f"[ws_client][recv] Connection closed: code={e.code} reason={e.reason}")
    except Exception as e:
        print(f"[ws_client][recv] Error: {e!r}")
    finally:
        print("[ws_client] Receiver loop finished.")


async def send_from_queue(
    ws,
    state,
    msg_queue: Queue,
    *,
    buffer_messages: bool = True,
    compress_messages: bool = True,
    max_buffer_size: int = 30,
):
    """
    Pulls messages from the queue and sends them to the websocket server.
    """
    print("[ws_client] Sender loop started.")
    try:
        buffer = []

        while True:
            try:
                if (s := msg_queue.qsize()) > 1:
                    print(f'[ws_client] inbound message queue size: {s}')
                payload = msg_queue.get_nowait()

                if payload is None:
                    print("[ws_client][send] Shutdown signal received.")
                    break

                # TODO: send message to websocket server if it's been too longs since we've gotten a message from the queue
                if state.get("require_key") or state.get("always_include_key"):
                    if state.get("producer_key"):
                        if isinstance(payload, dict):
                            payload["key"] = state["producer_key"]
                        else:
                            print(f"[ws_client][warn] Cannot add key to non-dict payload: {payload!r}")

                if buffer_messages:
                    buffer.append(payload)

                if payload['game_ended_this_tick'] or len(buffer) >= max_buffer_size or not buffer_messages:

                    if buffer_messages:
                        message = dict(meta={}, payload=buffer)
                    else:
                        message = payload

                    if compress_messages:
                        compressor = zstd.ZstdCompressor(level=1)
                        message_bytes = compressor.compress(orjson.dumps(message, option=orjson.OPT_NON_STR_KEYS))
                        message = base64.b64encode(message_bytes).decode()
                    else:
                        message = orjson.dumps(message, option=orjson.OPT_NON_STR_KEYS).decode()

                    await ws.send(message)
                    buffer = []

            except Empty:
                await asyncio.sleep(0.01)
            except websockets.ConnectionClosed:
                print("[ws_client][send] Connection closed while sending.")
                break
    finally:
        print("[ws_client] Sender loop finished.")


async def run_client(msg_queue: Queue, host: str, room: str, **kwargs: Unpack[ClientKwargs]):
    """
    The main async function that manages the connection and tasks.
    """
    preempt_key = kwargs.get("preempt_key", None)
    always_include_key = kwargs.get("always_include_key", False)

    uri = f"{host}/ws/{quote(room)}?role=producer"

    headers = {}
    if preempt_key:
        headers["Authorization"] = f"Bearer {preempt_key}"

    state = {
        "producer_key": None,
        "require_key": False,
        "always_include_key": always_include_key,
    }

    while True:
        try:
            print(f"[ws_client][info] Attempting to connect to {uri}...")
            async with websockets.connect(uri, additional_headers=headers, ping_interval=20, ping_timeout=20) as ws:
                print(f"[ws_client][info] Connection established.")

                raw = await ws.recv()
                try:
                    welcome = json.loads(raw)
                    if welcome.get("type") == "welcome" and welcome.get("role") == "producer":
                        state["producer_key"] = welcome.get("producerKey")
                        expires_at = welcome.get("expiresAt")
                        print(f"[ws_client][info] Welcome: key={state['producer_key']} expiresAt={expires_at}")
                    else:
                        print(f"[ws_client][warn] Unexpected welcome message: {welcome}")
                except (json.JSONDecodeError, AttributeError):
                    print(f"[ws_client][warn] Unexpected first message: {raw!r}")

                recv_task = asyncio.create_task(recv_loop(ws, state))
                send_task = asyncio.create_task(send_from_queue(ws, state, msg_queue, **kwargs))

                done, pending = await asyncio.wait(
                    [recv_task, send_task],
                    return_when=asyncio.FIRST_COMPLETED
                )

                if send_task in done and msg_queue.empty():
                    print("[ws_client][info] Shutting down permanently.")
                    for task in pending:
                        task.cancel()
                    break

        except (websockets.exceptions.ConnectionClosedError, ConnectionRefusedError) as e:
            print(f"[ws_client][error] Connection failed: {e}. Retrying in 5 seconds...")
        except Exception as e:
            print(f"[ws_client][error] An unexpected error occurred: {e!r}. Retrying in 5 seconds...")

        await asyncio.sleep(5)


def start_client(msg_queue: Queue, host: str = "ws://127.0.0.1:8787", room: str = "test-room", **kwargs: Unpack[ClientKwargs]):
    """
    Entry point to be called in a background thread.
    Sets up and runs the asyncio event loop for the websocket client.
    """
    print(f"[ws_client] Thread starting for room '{room}'.")
    try:
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

        loop.run_until_complete(run_client(msg_queue, host, room, **kwargs))
    finally:
        print("[ws_client] Event loop closed. Thread finished.")