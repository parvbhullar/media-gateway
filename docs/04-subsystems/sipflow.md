# SipFlow

## What it does

The sipflow module provides unified SIP+RTP packet capture and storage.
It records SIP messages and RTP statistics for each call, enabling
post-call diagnostics, call flow visualization, and media quality
analysis. It supports both local (SQLite-backed) and remote (UDP/HTTP)
storage backends.

## Key types & entry points

- **`SipFlowBackend`** (trait) — pluggable backend for recording and querying SIP flow data. Methods: `record()`, `flush()`, `query_flow()`, `query_media_stats()`, `query_media()`. `src/sipflow/backend/mod.rs`
- **`SipFlowItem`** — a single captured SIP or RTP record: timestamp, sequence, msg type (Sip/Rtp), source/dest addresses, payload. `src/sipflow/mod.rs`
- **`SipFlowQuery`** — query interface wrapping a backend; provides `get_flow()`, `get_media()`, and `export_jsonl()`. `src/sipflow/mod.rs`
- **`StorageManager`** — local backend storage engine using SQLite + raw files, with batched writes, LRU call-ID cache, and hourly file rotation. `src/sipflow/storage.rs`
- **`SipFlowMsgType`** (enum) — `Sip` or `Rtp`. `src/sipflow/mod.rs`

## Sub-modules

- `backend/` — Backend trait and implementations
  - `backend/local.rs` — Local SQLite-based backend (`LocalBackend`)
  - `backend/remote.rs` — Remote UDP/HTTP backend (`RemoteBackend`)
- `protocol.rs` — Packet parsing (`MsgType`, `Packet`, `parse_packet`)
- `storage.rs` — `StorageManager` for local file/DB storage
- `sdp_utils.rs` — SDP parsing helpers (`extract_call_id`, `extract_rtp_addr`, `extract_sdp`)
- `wav_utils.rs` — WAV file utilities for media capture

## Configuration

Config section `[sipflow]` with variants:

- **Local:** `root` (storage directory), `subdirs` (directory structure), `flush_count`, `flush_interval_secs`, `id_cache_size`
- **Remote:** `udp_addr`, `http_addr`, `timeout_secs`

## Public API surface

SipFlow data is queried through the handler layer (AMI/API) for call
detail views. The module itself does not register HTTP routes.

## See also

- [callrecord.md](callrecord.md) — CDR generation that references SipFlow data
- [handler.md](handler.md) — API endpoints that expose SipFlow queries
- [../05-integration/](../05-integration/) — External integrations

---
**Status:** ✅ Shipped
**Source:** `src/sipflow/`
**Related phases:** (core infrastructure)
**Last reviewed:** 2026-04-16
