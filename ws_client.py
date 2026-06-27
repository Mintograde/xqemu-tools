import asyncio
import base64
import copy
import datetime
import json
import urllib.request
from queue import Queue, Empty
import time
from typing import Unpack, TypedDict
from urllib.parse import quote

import orjson
import websockets
import zstandard as zstd

from replay_utils import strip_tick

DEFAULT_SETTINGS = dict(
    host='ws://127.0.0.1:8787',
    room='test-room',
    buffer_messages=True,
    compress_messages=True,
    compress_messages_binary=True,
    compression_level=12,
    max_buffer_size=30,
    include_all_fields=False,
    send_live_status=True,
    live_status_interval_seconds=10,
)

LIVE_STATUS_TICKS_PER_SECOND = 30
LIVE_STATUS_TERMINAL_STATUSES = {"postgame", "ended", "stale"}


class ClientBaseKwargs(TypedDict, total=False):
    preempt_key: str
    always_include_key: bool


class SendKwargs(TypedDict, total=False):
    buffer_messages: bool
    compress_messages: bool
    compress_messages_binary: bool
    compression_level: int
    max_buffer_size: int
    include_all_fields: bool
    send_live_status: bool
    live_status_interval_seconds: int


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

            if isinstance(msg, dict) and msg.get("type") == "error" and msg.get("code") == "BAD_KEY":
                state["require_key"] = True
                print("[ws_client][info] Server requires per-message key. Will include it in subsequent messages.")
            elif isinstance(msg, dict) and msg.get("type") == "replay_upload_presign_response":
                print(f"[ws_client][recv] replay upload presign response request_id={msg.get('request_id')}")
                await handle_replay_upload_presign_response(ws, state, msg)
            elif isinstance(msg, dict) and msg.get("type") == "replay_upload_presign_error":
                print(f"[ws_client][recv] replay upload presign error request_id={msg.get('request_id')} status={msg.get('status')} error={msg.get('error')}")
            else:
                print(f"[ws_client][recv] {msg}")
    except websockets.ConnectionClosed as e:
        print(f"[ws_client][recv] Connection closed: code={e.code} reason={e.reason}")
    except Exception as e:
        print(f"[ws_client][recv] Error: {e!r}")
    finally:
        print("[ws_client] Receiver loop finished.")


def _optional_string(value):
    if value is None:
        return None
    value = str(value).strip()
    return value or None


def _optional_int(value):
    if value is None or value == "":
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _isoformat(value):
    if value is None:
        return None
    if isinstance(value, datetime.datetime):
        return value.isoformat()
    return _optional_string(value)


def _game_status(game_info):
    if game_info.get("game_ended_this_tick"):
        return "ended"
    if game_info.get("game_engine_can_score"):
        return "live"
    if game_info.get("game_engine_running"):
        return "waiting"
    return "stale"


def _player_damage(game_info, player_index, field):
    try:
        return game_info["game_meta"]["players"][player_index][field]
    except (KeyError, TypeError):
        return 0


def _player_summary(game_info):
    players = []
    for player in game_info.get("players") or []:
        player_index = player.get("player_index")
        derived_stats = player.get("derived_stats") or {}
        players.append(dict(
            player_index=player_index,
            name=player.get("name"),
            team_index=player.get("team"),
            local_player=player.get("local_player"),
            score=player.get("score"),
            kills=player.get("kills"),
            deaths=player.get("deaths"),
            assists=player.get("assists"),
            team_kills=player.get("team_kills"),
            suicides=player.get("suicides"),
            respawn_timer=player.get("respawn_timer"),
            has_camo=bool(derived_stats.get("has_camo")),
            has_overshield=bool(derived_stats.get("has_overshield")),
            damage_dealt=_player_damage(game_info, player_index, "damage_dealt"),
            damage_received=_player_damage(game_info, player_index, "damage_received"),
        ))
    return players


def _team_summary(game_info, player_summary):
    if not game_info.get("game_engine_has_teams"):
        return []

    teams = {}
    for player in player_summary:
        team_index = player.get("team_index")
        if team_index is None:
            continue
        team = teams.setdefault(team_index, dict(
            team_index=team_index,
            player_count=0,
            score=0,
            kills=0,
            deaths=0,
        ))
        team["player_count"] += 1
        team["score"] += player.get("score") or 0
        team["kills"] += player.get("kills") or 0
        team["deaths"] += player.get("deaths") or 0

    return [teams[key] for key in sorted(teams)]


def build_live_status_message(game_info):
    game_time_info = game_info.get("game_time_info") or {}
    current_tick = _optional_int(game_time_info.get("game_time"))
    game_id = _optional_string(game_info.get("game_id"))
    player_summary = _player_summary(game_info)
    map_info = game_info.get("map_info") or {}
    map_resolution_inputs = game_info.get("map_resolution_inputs") or {}
    map_resolution_map_info = map_resolution_inputs.get("map_info") or {}
    spawn_parameters_hash = game_info.get("spawn_parameters_hash")
    spawn_points = map_resolution_inputs.get("spawn_points") or []
    build_version = map_resolution_map_info.get("build_version") or map_info.get("build_version")
    cache_version = map_resolution_map_info.get("cache_version") or map_info.get("cache_version")

    return dict(
        type="live_status",
        status=_game_status(game_info),
        source_external_id=game_id,
        map_engine_name=map_resolution_inputs.get("map_engine_name") or game_info.get("multiplayer_map_name"),
        build_version=build_version,
        cache_version=cache_version,
        spawn_parameters_hash=spawn_parameters_hash,
        map_resolution_inputs=map_resolution_inputs,
        spawn_points=spawn_points,
        game_type=_optional_string(game_info.get("game_type")),
        variant=game_info.get("variant"),
        variant_name=_optional_string(game_info.get("global_stage")),
        started_at=_isoformat(game_info.get("start_time")),
        observed_at=_isoformat(game_info.get("current_time")),
        current_game_time_seconds=(
            current_tick / LIVE_STATUS_TICKS_PER_SECOND
            if current_tick is not None and current_tick >= 0
            else None
        ),
        current_tick=current_tick,
        player_summary=player_summary,
        team_summary=_team_summary(game_info, player_summary),
        raw_status=dict(
            game_engine_running=game_info.get("game_engine_running"),
            game_engine_can_score=game_info.get("game_engine_can_score"),
            game_ended_this_tick=game_info.get("game_ended_this_tick"),
        ),
        game_metadata=dict(
            source="xqemu-tools",
            legacy_game_id=game_id,
            map_name=game_info.get("multiplayer_map_name"),
            map_info=dict(
                scenario_name=map_info.get("scenario_name"),
                checksum=map_info.get("checksum"),
                build_version=map_info.get("build_version"),
                cache_version=map_info.get("cache_version"),
            ),
            game_type=game_info.get("game_type"),
            variant=game_info.get("variant"),
            game_engine_has_teams=game_info.get("game_engine_has_teams"),
            spawn_parameters_hash=spawn_parameters_hash,
            map_resolution_inputs=map_resolution_inputs,
        ),
    )


def _add_key_if_needed(payload, state):
    if state.get("require_key") or state.get("always_include_key"):
        if state.get("producer_key"):
            if isinstance(payload, dict):
                payload["key"] = state["producer_key"]
            else:
                print(f"[ws_client][warn] Cannot add key to non-dict payload: {payload!r}")


def _upload_replay_file(path, presigned_request):
    with open(path, "rb") as f:
        data = f.read()
    req = urllib.request.Request(
        presigned_request["url"],
        data=data,
        method=presigned_request.get("method", "PUT"),
        headers=presigned_request.get("headers") or {},
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return resp.status


def _safe_upload_error(e):
    status = getattr(e, "code", None) or getattr(e, "status", None)
    reason = getattr(e, "reason", None)
    if status:
        return f"status={status} reason={reason}"
    return e.__class__.__name__


async def handle_replay_upload_presign_response(ws, state, msg):
    request_id = msg.get("request_id")
    pending = state.get("replay_uploads", {}).get(request_id)
    if not pending:
        print(f"[ws_client][upload] no local replay path for request_id={request_id}")
        return
    try:
        status = await asyncio.to_thread(_upload_replay_file, pending["path"], msg["presigned_request"])
        upload_id = (msg.get("upload") or {}).get("id")
        ack = dict(type="replay_upload_client_status", request_id=request_id, upload_id=upload_id, status="uploaded")
        _add_key_if_needed(ack, state)
        await ws.send(orjson.dumps(ack).decode())
        state["replay_uploads"].pop(request_id, None)
        print(f"[ws_client][upload] replay uploaded request_id={request_id} status={status}")
    except Exception as e:
        pending["attempts"] = pending.get("attempts", 0) + 1
        print(f"[ws_client][upload] replay upload failed request_id={request_id}: {_safe_upload_error(e)}")
        if pending["attempts"] <= state.get("max_replay_upload_retries", 2):
            await asyncio.sleep(min(30, 2 ** pending["attempts"]))
            retry_payload = pending["payload"].copy()
            _add_key_if_needed(retry_payload, state)
            await ws.send(orjson.dumps(retry_payload).decode())
        else:
            state["replay_uploads"].pop(request_id, None)
            print(f"[ws_client][upload] replay upload giving up request_id={request_id}")


async def send_live_status_message(ws, state, live_status):
    message = copy.deepcopy(live_status)
    message["observed_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    _add_key_if_needed(message, state)
    await ws.send(orjson.dumps(message).decode())


async def send_from_queue(
    ws,
    state,
    msg_queue: Queue,
    *,
    buffer_messages: bool = DEFAULT_SETTINGS["buffer_messages"],
    compress_messages: bool = DEFAULT_SETTINGS["compress_messages"],
    compress_messages_binary: bool = DEFAULT_SETTINGS["compress_messages_binary"],
    compression_level: int = DEFAULT_SETTINGS["compression_level"],
    max_buffer_size: int = DEFAULT_SETTINGS["max_buffer_size"],
    include_all_fields: bool = DEFAULT_SETTINGS["include_all_fields"],
    send_live_status: bool = DEFAULT_SETTINGS["send_live_status"],
    live_status_interval_seconds: int = DEFAULT_SETTINGS["live_status_interval_seconds"],
    **kwargs,
):
    """
    Pulls messages from the queue and sends them to the websocket server.
    """
    print("[ws_client] Sender loop started.")
    try:
        buffer = []
        last_live_status = None
        last_live_status_sent_at = 0.0
        last_live_status_game_id = None
        last_live_status_spawn_parameters_hash = None
        terminal_status_sent_for_game_id = None

        while True:
            try:
                if (s := msg_queue.qsize()) > 1:
                    print(f'[ws_client] inbound message queue size: {s}')
                payload = msg_queue.get_nowait()

                if payload is None:
                    print("[ws_client][send] Shutdown signal received.")
                    break

                if isinstance(payload, dict) and payload.get("type") == "replay_upload_presign_request":
                    local_path = payload.pop("_local_file_path", None)
                    request_id = payload.get("request_id")
                    if local_path and request_id:
                        state.setdefault("replay_uploads", {})[request_id] = dict(path=local_path, payload=payload.copy(), attempts=0)
                    _add_key_if_needed(payload, state)
                    await ws.send(orjson.dumps(payload).decode())
                    continue

                if send_live_status:
                    live_status = build_live_status_message(payload)
                    live_status_game_id = (
                        live_status.get("source_external_id")
                        or live_status.get("started_at")
                        or "__unknown__"
                    )
                    if live_status_game_id != last_live_status_game_id:
                        terminal_status_sent_for_game_id = None
                        last_live_status_game_id = live_status_game_id
                        last_live_status_spawn_parameters_hash = None

                    last_live_status = live_status
                    live_status_is_terminal = live_status.get("status") in LIVE_STATUS_TERMINAL_STATUSES
                    live_status_spawn_parameters_hash = live_status.get("spawn_parameters_hash")
                    live_status_spawn_parameters_changed = (
                        live_status_spawn_parameters_hash is not None
                        and live_status_spawn_parameters_hash != last_live_status_spawn_parameters_hash
                    )
                    live_status_due = (
                        time.monotonic() >= last_live_status_sent_at + live_status_interval_seconds
                    )
                    terminal_status_due = (
                        live_status_is_terminal
                        and terminal_status_sent_for_game_id != live_status_game_id
                    )

                    if (
                        terminal_status_due
                        or live_status_spawn_parameters_changed
                        or (live_status_due and not live_status_is_terminal)
                    ):
                        await send_live_status_message(ws, state, live_status)
                        last_live_status_sent_at = time.monotonic()
                        if live_status_spawn_parameters_hash is not None:
                            last_live_status_spawn_parameters_hash = live_status_spawn_parameters_hash
                        if live_status_is_terminal:
                            terminal_status_sent_for_game_id = live_status_game_id

                if not include_all_fields:
                    payload_copy = copy.deepcopy(payload)
                    payload = strip_tick(payload_copy)

                _add_key_if_needed(payload, state)

                if buffer_messages:
                    buffer.append(payload)

                if payload['game_ended_this_tick'] or len(buffer) >= max_buffer_size or not buffer_messages:

                    if buffer_messages:
                        message = dict(meta={}, payload=buffer)
                    else:
                        message = payload

                    if compress_messages:
                        # compressed size (1v1 single-tick message):
                        #   level 1: 9.3 kB; level 12: 8.5 kB; level 22: 8.0 kB
                        # processing time (1v1 single-tick message):
                        #   level 1: 0.001366s; level 12: 0.001827s; level 22: 0.009744s
                        compressor = zstd.ZstdCompressor(level=compression_level)
                        message_bytes = compressor.compress(orjson.dumps(message, option=orjson.OPT_NON_STR_KEYS))
                        if compress_messages_binary:
                            message = message_bytes
                        else:
                            message = base64.b64encode(message_bytes).decode()
                    else:
                        message = orjson.dumps(message, option=orjson.OPT_NON_STR_KEYS).decode()

                    await ws.send(message)
                    buffer = []

            except Empty:
                if (
                    send_live_status
                    and last_live_status
                    and last_live_status.get("status") not in LIVE_STATUS_TERMINAL_STATUSES
                    and time.monotonic() >= last_live_status_sent_at + live_status_interval_seconds
                ):
                    await send_live_status_message(ws, state, last_live_status)
                    last_live_status_sent_at = time.monotonic()
                    if last_live_status.get("spawn_parameters_hash") is not None:
                        last_live_status_spawn_parameters_hash = last_live_status.get("spawn_parameters_hash")
                await asyncio.sleep(0.01)
            except websockets.ConnectionClosed:
                print("[ws_client][send] Connection closed while sending.")
                break
            except Exception as e:
                print(f"[ws_client][send] Unexpected error: {e!r}")
                break
    finally:
        print("[ws_client] Sender loop finished.")


async def run_client(msg_queue: Queue, host: str, room: str, **kwargs: Unpack[ClientKwargs]):
    """
    The main async function that manages the connection and tasks.
    """
    preempt_key = kwargs.get("preempt_key", None)
    always_include_key = kwargs.get("always_include_key", False)
    buffer_messages = kwargs.get("buffer_messages", DEFAULT_SETTINGS["buffer_messages"])
    compress_messages = kwargs.get("compress_messages", DEFAULT_SETTINGS["compress_messages"])
    compress_messages_binary = kwargs.get("compress_messages_binary", DEFAULT_SETTINGS["compress_messages_binary"])

    uri = (
        f"{host}/ws/{quote(room)}?role=producer"
        f"&compress_messages={quote(str(compress_messages))}"
        f"&compress_messages_binary={quote(str(compress_messages_binary))}"
        f"&buffer_messages={quote(str(buffer_messages))}"
    )

    headers = {}
    if preempt_key:
        headers["Authorization"] = f"Bearer {preempt_key}"

    state = {
        "producer_key": None,
        "require_key": False,
        "always_include_key": always_include_key,
        "replay_uploads": {},
        "max_replay_upload_retries": 2,
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
                        print(f"[ws_client][info] Welcome: producer key received expiresAt={expires_at}")
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

        dropped = 0
        retained_uploads = []
        retained_upload_request_ids = set()
        while not msg_queue.empty():
            try:
                payload = msg_queue.get_nowait()
                if isinstance(payload, dict) and payload.get("type") == "replay_upload_presign_request":
                    retained_uploads.append(payload)
                    if payload.get("request_id"):
                        retained_upload_request_ids.add(payload["request_id"])
                else:
                    dropped += 1
            except Empty:
                break
        for payload in retained_uploads:
            msg_queue.put(payload)
        requeued = 0
        for request_id, pending in state.get("replay_uploads", {}).items():
            if request_id not in retained_upload_request_ids:
                msg_queue.put(pending["payload"].copy())
                requeued += 1

        if dropped > 0:
            print(f"[ws_client][warn] Disconnected: Flushed {dropped} stale messages from queue.")
        if requeued > 0:
            print(f"[ws_client][warn] Disconnected: Requeued {requeued} pending replay upload requests.")

        await asyncio.sleep(5)


def start_client(msg_queue: Queue, host: str = DEFAULT_SETTINGS['host'], room: str = DEFAULT_SETTINGS['room'], **kwargs: Unpack[ClientKwargs]):
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
