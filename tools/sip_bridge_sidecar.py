"""
SIP↔LiveKit bridge sidecar — the Python half of rustpbx's
`kind="external_media"` bridge.

rustpbx terminates SIP + RTP and pipes decoded audio to this process over a
localhost UDP socket; this process owns ALL LiveKit logic (joining the room,
dispatching the agent, publishing the caller's audio, subscribing to the
agent's audio). This decouples rustpbx from the flaky Rust LiveKit SDK — the
Python SDK has proven reliable on the same machine/minute where the Rust SDK
fails `wait_pc_connection`.

Wire protocol (both directions), matching rustpbx's external_media::media:
  * raw 16-bit little-endian PCM, 48 kHz, mono, 20 ms frames = 1920 bytes.
  * control datagrams: b"READY" (sidecar→rustpbx, once, on join) and
    b"BYE" (either direction, to end the call).

Lifecycle:
  1. rustpbx spawns:  <command> --call-id <id> --did <to> --caller <from>
                      --port <Q>
  2. Sidecar binds an ephemeral 127.0.0.1 UDP socket, sends b"READY" to
     127.0.0.1:Q. rustpbx learns our address from that datagram and
     `connect()`s its socket to us, then answers the SIP INVITE.
  3. Sidecar joins LiveKit room, dispatches the agent, bridges audio.
  4. On room disconnect / agent leave / SIGTERM / rustpbx b"BYE", the sidecar
     sends b"BYE" to rustpbx (best-effort) and exits.

LiveKit credentials come from this process's own environment
(LIVEKIT_URL / LIVEKIT_API_KEY / LIVEKIT_API_SECRET / AGENT_NAME) — they are
intentionally NOT passed by rustpbx, keeping the trunk's kind_config minimal.

Usage (normally invoked by rustpbx, but runnable by hand for testing):
  python sip_bridge_sidecar.py --call-id abc --did 1000 --caller 2000 --port 41234
"""

import argparse
import asyncio
import logging
import os
import re
import signal

import numpy as np
from dotenv import find_dotenv, load_dotenv
from livekit import api, rtc

# Load LiveKit creds (LIVEKIT_URL / LIVEKIT_API_KEY / LIVEKIT_API_SECRET /
# AGENT_NAME). Search order, first hit wins:
#   1. $SIP_SIDECAR_ENV (explicit override),
#   2. <this script's dir>/.env,
#   3. nearest .env walking up from the spawning CWD (rustpbx's dir).
# rustpbx spawns us with its own CWD (media-gateway), where the deploy keeps
# these keys; env vars already in the process environment always win.
_loaded = False
for _cand in (os.getenv("SIP_SIDECAR_ENV"),
              os.path.join(os.path.dirname(os.path.abspath(__file__)), ".env")):
    if _cand and os.path.isfile(_cand) and load_dotenv(_cand):
        _loaded = True
        break
if not _loaded:
    load_dotenv(find_dotenv(usecwd=True))
logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
log = logging.getLogger("sip-sidecar")

SR = 48000               # LiveKit native sample rate (== rustpbx wire rate)
CH = 1
FRAME_MS = 20            # rustpbx sends/expects 20 ms frames
FRAME_SAMPLES = SR * FRAME_MS // 1000      # 960
FRAME_BYTES = FRAME_SAMPLES * 2            # 1920

READY = b"READY"
BYE = b"BYE"


def room_name_for(call_id: str, did: str) -> str:
    """Derive a per-call room name. call_id is unique per call; sanitise it
    to the charset LiveKit room names allow."""
    base = call_id or f"sip-{did}"
    safe = re.sub(r"[^A-Za-z0-9._-]", "-", base)
    return f"sip-{safe}"


class PcmBridgeProtocol(asyncio.DatagramProtocol):
    """Receives caller PCM (rustpbx→sidecar) and feeds it to the LiveKit
    publisher source in order. Also the send path for agent PCM."""

    def __init__(self, source: rtc.AudioSource, queue: asyncio.Queue, stop: asyncio.Event):
        self.source = source
        self.queue = queue
        self.stop = stop
        self.transport: asyncio.DatagramTransport | None = None

    def connection_made(self, transport: asyncio.DatagramTransport) -> None:
        self.transport = transport

    def datagram_received(self, data: bytes, addr) -> None:
        if data == BYE:
            log.info("rustpbx sent BYE — ending call")
            self.stop.set()
            return
        if len(data) == FRAME_BYTES:
            # Hand off to the ordered consumer; never await here.
            try:
                self.queue.put_nowait(data)
            except asyncio.QueueFull:
                # Drop oldest to bound latency under backpressure.
                try:
                    self.queue.get_nowait()
                    self.queue.put_nowait(data)
                except asyncio.QueueEmpty:
                    pass

    def error_received(self, exc) -> None:
        log.warning("PCM socket error: %s", exc)


async def feed_caller_audio(source: rtc.AudioSource, queue: asyncio.Queue,
                            stop: asyncio.Event) -> None:
    """Single ordered consumer: caller PCM datagrams → LiveKit capture_frame."""
    while not stop.is_set():
        try:
            data = await asyncio.wait_for(queue.get(), timeout=0.5)
        except asyncio.TimeoutError:
            continue
        frame = rtc.AudioFrame(
            data=data,
            sample_rate=SR,
            num_channels=CH,
            samples_per_channel=FRAME_SAMPLES,
        )
        await source.capture_frame(frame)


async def forward_agent_audio(track: rtc.RemoteAudioTrack, transport: asyncio.DatagramTransport,
                              dst, stop: asyncio.Event, ready: asyncio.Event) -> None:
    """Agent audio track → reframe to 20 ms (1920 B) → rustpbx over UDP.

    AudioStream is constructed at 48 kHz mono so frames arrive already
    resampled/downmixed; we only need to re-chunk to exactly 1920 bytes.

    Waits for `ready` (set once READY has been sent to rustpbx) before
    forwarding, so rustpbx's first received datagram is always READY — not
    a stray PCM frame from a track that subscribed during room.connect()."""
    await ready.wait()
    stream = rtc.AudioStream(track, sample_rate=SR, num_channels=CH)
    buf = bytearray()
    sent = 0
    async for ev in stream:
        if stop.is_set():
            break
        buf.extend(ev.frame.data)
        while len(buf) >= FRAME_BYTES:
            chunk = bytes(buf[:FRAME_BYTES])
            del buf[:FRAME_BYTES]
            transport.sendto(chunk, dst)
            sent += 1
            if sent == 1:
                log.info("first agent→rustpbx frame sent")
    await stream.aclose()


async def main(call_id: str, did: str, caller: str, port: int) -> None:
    url = os.environ["LIVEKIT_URL"]
    key = os.environ["LIVEKIT_API_KEY"]
    secret = os.environ["LIVEKIT_API_SECRET"]
    agent_name = os.getenv("AGENT_NAME", "Aria")
    room_name = room_name_for(call_id, did)

    loop = asyncio.get_running_loop()
    stop = asyncio.Event()
    ready = asyncio.Event()   # set once READY is sent; gates agent forwarding

    # Graceful shutdown on SIGTERM/SIGINT (rustpbx teardown force-kills after
    # a grace period; we want to leave the room cleanly within it).
    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, stop.set)
        except NotImplementedError:
            pass

    # --- 1. bind our side of the PCM pipe (READY is sent later, only once
    #        the LiveKit room is joined + caller track published, so rustpbx
    #        never answers the SIP call into a dead bridge). ---
    rustpbx_addr = ("127.0.0.1", port)
    source = rtc.AudioSource(SR, CH)
    queue: asyncio.Queue = asyncio.Queue(maxsize=50)
    transport, protocol = await loop.create_datagram_endpoint(
        lambda: PcmBridgeProtocol(source, queue, stop),
        local_addr=("127.0.0.1", 0),
    )
    log.info("sidecar bound %s → rustpbx %s (call_id=%s did=%s caller=%s room=%s)",
             transport.get_extra_info("sockname"), rustpbx_addr, call_id, did, caller, room_name)

    # --- 2. dispatch the agent into the room ---
    async with api.LiveKitAPI(url=url, api_key=key, api_secret=secret) as lk:
        d = await lk.agent_dispatch.create_dispatch(
            api.CreateAgentDispatchRequest(agent_name=agent_name, room=room_name)
        )
        log.info("dispatched agent '%s' → room '%s' (id=%s)", agent_name, room_name, d.id)

    # --- 3. join the room as the caller participant ---
    token = (
        api.AccessToken(key, secret)
        .with_identity(f"sip-caller-{caller}")
        .with_name(f"sip-caller-{caller}")
        .with_grants(api.VideoGrants(room_join=True, room=room_name))
        .to_jwt()
    )
    room = rtc.Room()

    @room.on("track_subscribed")
    def _ts(track: rtc.Track, pub, participant: rtc.RemoteParticipant):
        if track.kind == rtc.TrackKind.KIND_AUDIO:
            log.info("subscribed to agent audio from %s — forwarding to rustpbx",
                     participant.identity)
            asyncio.create_task(forward_agent_audio(track, transport, rustpbx_addr, stop, ready))

    @room.on("disconnected")
    def _disc(reason=None):
        log.info("room disconnected (reason=%s)", reason)
        stop.set()

    await room.connect(url, token)
    log.info("connected to room; remote participants=%d", len(room.remote_participants))

    # --- 4. publish caller audio (fed from rustpbx PCM) ---
    track = rtc.LocalAudioTrack.create_audio_track("sip-caller-audio", source)
    await room.local_participant.publish_track(
        track, rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE))
    feeder = asyncio.create_task(feed_caller_audio(source, queue, stop))
    log.info("publishing caller audio; bridge live")

    # Bridge is fully up (room joined + caller track published). Signal
    # rustpbx to answer the SIP INVITE and start pumping PCM.
    transport.sendto(READY, rustpbx_addr)
    ready.set()   # release any agent-audio forwarders that subscribed early
    log.info("READY → rustpbx %s", rustpbx_addr)

    # --- 5. run until stop, then tear down ---
    await stop.wait()
    log.info("tearing down sidecar")
    feeder.cancel()
    transport.sendto(BYE, rustpbx_addr)   # best-effort notify rustpbx
    try:
        await room.disconnect()
    except Exception as e:  # noqa: BLE001
        log.warning("room.disconnect error: %s", e)
    transport.close()


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--call-id", dest="call_id", required=True)
    ap.add_argument("--did", required=True)
    ap.add_argument("--caller", required=True)
    ap.add_argument("--port", type=int, required=True)
    args = ap.parse_args()
    asyncio.run(main(args.call_id, args.did, args.caller, args.port))
