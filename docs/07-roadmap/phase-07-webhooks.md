# Phase 7: Webhook Pipeline

## Goal

Ship a CRUD webhook registry plus a background processor that delivers CDR completion events with HMAC signing, retries, and disk fallback.

## Dependencies

Phase 1.

## Requirements

- **WH-01**: A new `rustpbx_webhooks` table + CRUD endpoints at `/api/v1/webhooks` exist
- **WH-02**: A background processor consumes `callrecord/` completion events and delivers them to registered webhooks
- **WH-03**: Webhook delivery posts JSON with HMAC header using the webhook's secret, uses 3 retries with exponential backoff, and falls back to a disk JSON file when all retries fail
- **WH-04**: Webhook events include `X-Webhook-Event`, `X-Webhook-Secret`, and a request id header
- **WH-05**: Creating a webhook fires a test event synchronously; failure to deliver the test is non-fatal and logged
- **WH-06**: `PUT /api/v1/webhooks/{id}` updates a webhook; `DELETE /api/v1/webhooks/{id}` removes it and cancels any in-flight retries

## Success Criteria

1. Operator can CRUD webhooks via `/api/v1/webhooks` and each new webhook fires a synchronous test event whose failure is logged but non-fatal
2. Completion of a call in `callrecord/` triggers webhook delivery with JSON payload, HMAC header, and the documented `X-Webhook-Event`/`X-Webhook-Secret`/request-id headers
3. A failing webhook target is retried 3 times with exponential backoff and then written to a disk JSON fallback under `ProxyConfig.generated_dir`
4. Deleting a webhook cancels any in-flight retries for that endpoint

## Affected Subsystems

- [handler](../04-subsystems/)
- [callrecord](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/07-webhook-pipeline/`
**Last reviewed:** 2026-04-16
