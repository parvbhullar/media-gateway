# Recording Model

SuperSip captures call data at two levels: **SipFlow** for raw
SIP/RTP packet capture, and **Call Records** for structured CDR data.
This page describes both systems and how they relate.

## SipFlow: unified SIP + RTP capture

SipFlow (`sipflow/mod.rs`) captures every SIP message and RTP packet
associated with a call into a queryable store. Each captured item is a
`SipFlowItem` containing:

| Field       | Description                              |
|-------------|------------------------------------------|
| `timestamp` | Microsecond-precision capture time        |
| `seq`       | Monotonic sequence number                 |
| `msg_type`  | `Sip` or `Rtp`                            |
| `src_addr`  | Source IP:port                             |
| `dst_addr`  | Destination IP:port                        |
| `payload`   | Raw packet bytes                           |

### Storage backends

SipFlow writes to pluggable backends (`sipflow/backend.rs`):

- **Local file backend** — writes hourly rotated binary files to disk.
  Files are organized by time window for efficient retrieval.
- **S3 backend** — uploads capture files to S3-compatible object storage
  (AWS S3, MinIO, Cloudflare R2).

### Query interface

`SipFlowQuery` provides a read API that retrieves flows by call ID and
time range. Results can be exported as JSONL for external analysis.
The `sdp_utils` module extracts call IDs, RTP addresses, and SDP bodies
from captured packets for correlation.

## Call records (CDR)

The `callrecord` module (`callrecord/mod.rs`) generates structured call
detail records. Each `CallDetails` record includes:

| Field                  | Description                          |
|------------------------|--------------------------------------|
| `direction`            | Inbound or outbound                   |
| `status`               | Final call disposition                |
| `from_number`          | Caller number                         |
| `to_number`            | Called number                         |
| `caller_name`          | Caller display name                   |
| `agent_name`           | Assigned agent (if queued)            |
| `queue`                | Queue name (if applicable)            |
| `sip_trunk_id`         | Originating/terminating trunk         |
| `recording_url`        | URL to the call recording             |
| `recording_duration_secs` | Recording length                   |
| `has_transcript`       | Whether transcription is available    |
| `rewrite`              | Original and final caller/callee after routing rewrites |
| `metadata`             | Arbitrary key-value pairs             |

### CDR pipeline

Call records flow through a channel-based pipeline:

1. **CallSession** emits events as the call progresses (ring, answer,
   bridge, hangup).
2. **CallRecordSender** (an mpsc channel) delivers events to the
   background writer.
3. The writer serializes records and stores them according to the
   configured backend.

### Recording hooks

The `CallRecordHook` trait allows plugins to intercept call records
after they are finalized. Hooks receive the complete `CallDetails` and
can perform additional processing — for example, posting to an external
analytics system or triggering post-call workflows.

Multiple hooks can be registered on the `SipServer` and run concurrently
after each call completes.

## Storage backends

Recording audio files are stored via the `Storage` abstraction
(`storage/mod.rs`):

| Backend   | Configuration                              |
|-----------|--------------------------------------------|
| **Local** | Writes WAV files to a configured directory  |
| **S3**    | Uploads to S3-compatible storage; supports AWS, MinIO, R2 |

The S3 backend supports multiple vendor configurations (`S3Vendor`)
with vendor-specific endpoint and authentication handling.

## Upload retry scheduler

The `upload_retry` module (`upload_retry/mod.rs`) ensures recordings
reach S3 even when uploads fail transiently:

- Runs on a 60-second tick, processing up to 50 pending rows per sweep.
- Failed uploads are retried with exponential backoff.
- After 10 attempts, a row is marked `failed_permanent` and skipped.
- If the source file is missing, the row is marked `failed_missing_source`.
- Local files are **never** deleted on permanent failure — operators can
  always recover manually.

## Transcription

SuperSip integrates with **SenseVoice** for automatic speech-to-text
transcription of recorded calls. Transcription status and language are
tracked in `CallDetails` (`has_transcript`, `transcript_status`,
`transcript_language`).

## Roadmap

| Phase | Feature                          | Status   |
|-------|----------------------------------|----------|
| 7     | Webhook dispatch for CDR events  | Planned  |
| 11    | CDR export API                   | Planned  |
| 12    | Recordings CRUD API              | Planned  |

## Further reading

- [SipFlow subsystem](../04-subsystems/sipflow.md) — packet capture details
- [Call Record subsystem](../04-subsystems/callrecord.md) — CDR internals
- [RWI Model](rwi-model.md) — recording control via RWI commands
