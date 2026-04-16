# Upload Retry

## What it does

The upload-retry module is a background scheduler that retries failed S3
uploads. When callrecord's S3 upload fails (network error, service
unavailable), the failure is recorded in the `pending_uploads` database
table. This scheduler wakes every 60 seconds, pulls a batch of eligible
rows, and re-attempts the upload using the current S3 configuration.

## Key types & entry points

- **`spawn()`** — starts the background retry scheduler task. Always safe to call; the loop is a no-op when callrecord is not configured for S3. `src/upload_retry/mod.rs`
- **`sweep()`** — pulls pending rows and retries them. Public so a future "Retry now" API endpoint can invoke it on demand. `src/upload_retry/mod.rs`

## Sub-modules

None — single file module.

## Behaviour

- **Tick interval:** 60 seconds
- **Batch size:** 50 items per tick
- **Max attempts:** 10 (then marked `failed_permanent`)
- **Backoff:** Exponential per-row backoff doubling from 1 minute up to 24 hours
- **On success:** Row is deleted from `pending_uploads`; local media file is deleted if `keep_media_copy = false`; `call_record.recording_url` is updated to the S3 URL
- **On permanent failure:** Row is marked `failed_permanent`; local file is always kept for manual recovery
- **Missing source:** If the local file no longer exists, the row is marked `failed_missing_source` and never retried

## Configuration

No dedicated config section. The scheduler reads `[callrecord]` S3
settings to build the storage client. It only activates when callrecord
is configured with an S3 backend.

## Public API surface

None currently. The `sweep()` function is public for future "Retry now"
endpoint integration.

## See also

- [callrecord.md](callrecord.md) — CDR module that creates pending upload rows on failure
- [storage.md](storage.md) — Storage abstraction used for S3 uploads
- [models.md](models.md) — `pending_upload` entity definition

---
**Status:** ✅ Shipped
**Source:** `src/upload_retry/`
**Related phases:** (core infrastructure)
**Last reviewed:** 2026-04-16
