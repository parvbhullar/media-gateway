# Webhooks

Event delivery from SuperSip to your systems.

---

## Current State

SuperSip currently delivers CDR events via the call record hook system configured in `[callrecord]`. Two delivery modes are supported:

### HTTP CDR Push (Shipped)

Push call details and recording files immediately after a call ends.

```toml
[callrecord]
type = "http"
url = "https://your-api.com/pbx/cdr"
with_media = true
```

**Format:** `multipart/form-data`
- Field `calllog.json` -- the full CDR JSON payload.
- File `media_audio-0` -- the recording WAV/MP3 file (when `with_media = true`).

The CDR push fires once per completed call. There is no retry or HMAC signing in the current implementation -- the URL receives a best-effort POST.

### Locator Webhook (Shipped)

Real-time notification when SIP devices register or unregister.

```toml
[proxy.locator_webhook]
url = "https://your-api.com/pbx/events"
events = ["registered", "unregistered", "offline"]
```

Payload includes the AOR, contact address, transport, user-agent, and expiry.

See [HTTP Router](http-router.md) for full details on both hooks.

---

## Planned: Webhook Registry (Phase 7)

Phase 7 will add a full webhook management API that replaces the static config-file approach with a dynamic, CRUD-managed webhook registry.

### API Surface

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/v1/webhooks` | List registered webhooks |
| GET | `/api/v1/webhooks/{id}` | Get webhook by ID |
| POST | `/api/v1/webhooks` | Create webhook (fires test event) |
| PUT | `/api/v1/webhooks/{id}` | Update webhook |
| DELETE | `/api/v1/webhooks/{id}` | Delete webhook, cancel in-flight retries |

### Delivery Guarantees

- **HMAC signing** -- every delivery includes an `X-Webhook-Secret` HMAC header computed from the webhook's secret key.
- **Event headers** -- `X-Webhook-Event` identifies the event type; a unique request ID enables idempotent processing.
- **Retries** -- 3 retries with exponential backoff on transient failures (5xx, network timeout).
- **Disk fallback** -- on permanent failure after all retries, the payload is written as a JSON file under `generated_dir` for manual replay or audit.
- **Test event** -- creating a webhook fires a synchronous test event; failure is logged but non-fatal (WH-05).

### Event Types (Planned)

| Event | Trigger | Source |
|-------|---------|--------|
| `call.completed` | CDR finalized | `callrecord/` pipeline |
| `call.started` | INVITE answered | Active call registry |
| `registration.changed` | SIP REGISTER/expire | Locator |

### Requirements

- **WH-01** -- `rustpbx_webhooks` table + CRUD endpoints
- **WH-02** -- Background processor consumes `callrecord/` completion events
- **WH-03** -- HMAC signing, 3 retries, disk fallback
- **WH-04** -- `X-Webhook-Event`, `X-Webhook-Secret`, request ID headers
- **WH-05** -- Test event on creation (non-fatal)
- **WH-06** -- Update and delete with in-flight retry cancellation

See [Phase 7: Webhooks](../07-roadmap/phase-07-webhooks.md) for the full plan.

---

## See Also

- [HTTP Router](http-router.md) -- inbound call routing webhooks and CDR push details
- [Carrier API](carrier-api.md) -- the `/api/v1/*` REST surface including planned webhook endpoints
- [Recording Model](../03-concepts/recording-model.md) -- CDR pipeline and recording lifecycle

---
**Status:** Partial (CDR hooks shipped, webhook registry planned)
**Related phases:** [Phase 7](../07-roadmap/phase-07-webhooks.md)
**Last reviewed:** 2026-04-16
