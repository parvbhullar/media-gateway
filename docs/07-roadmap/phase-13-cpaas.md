# Phase 13: CPaaS Layer (Endpoints, Applications, Sub-Accounts)

## Goal

Ship the 29-route Vobiz-shaped CPaaS surface: SIP user-agent endpoints, XML Applications routing, and multi-tenant sub-accounts with per-request account scoping.

## Dependencies

Phase 12.

## Requirements

- **EPUA-01**: Operator can create a SIP user-agent endpoint via `POST /api/v1/endpoints` with username, password, alias, and optional application reference
- **EPUA-02**: Operator can retrieve, update, delete a user-agent endpoint by id
- **EPUA-03**: Operator can list endpoints with pagination, scoped to the caller's account
- **EPUA-04**: Endpoint exposes `sip_registered` status derived from the live registrar state
- **EPUA-05**: Endpoint CRUD uses the existing `proxy/user_extension.rs` / `registrar.rs` infrastructure without requiring new proxy modules
- **APP-01**: A new `rustpbx_applications` table + CRUD endpoints at `/api/v1/applications` exist
- **APP-02**: An application has `answer_url`, `hangup_url`, `message_url`, and optional auth headers
- **APP-03**: Operator can attach and detach phone numbers to an application via `POST/DELETE /api/v1/applications/{id}/numbers`
- **APP-04**: An incoming call whose routing target is an application fetches XML from the answer_url with a configurable timeout and executes the returned verbs through the existing `call/app/ivr*` runtime
- **APP-05**: Hangup events POST call completion data to the application's hangup_url
- **APP-06**: Application XML verb set includes at minimum `Play`, `Speak`, `Dial`, `Hangup`, `GetDigits`, `Record` — mapped to existing IVR runtime primitives
- **TEN-01**: A new `rustpbx_sub_accounts` table is introduced; every existing api_v1 record defaults to a `root` account
- **TEN-02**: Operator can CRUD sub-accounts via `/api/v1/sub-accounts` with name, enabled flag, and auto-generated auth credentials
- **TEN-03**: API keys from `models/api_key.rs` gain an `account_id` column so every request resolves to an account scope
- **TEN-04**: Every api_v1 route that reads or writes account-scoped resources filters by the caller's account_id
- **TEN-05**: Master account sees all sub-accounts' resources via an explicit query parameter; sub-accounts cannot see sibling data
- **TEN-06**: The migration for sub-accounts is additive; all existing rows receive the root account_id
- **IT-04**: Applications XML answer-URL flow has an end-to-end test using a mock HTTP server returning canned XML
- **IT-05**: Sub-account isolation has a test asserting that a sub-account Bearer token cannot read or mutate another sub-account's trunk, DID, webhook, or recording

## Success Criteria

1. Tenant developer can CRUD SIP user-agent endpoints via `/api/v1/endpoints` and see live `sip_registered` status derived from the registrar — all wrapping existing `proxy/user_extension.rs` and `registrar.rs`
2. Tenant developer can CRUD Applications with `answer_url`/`hangup_url`/`message_url`, attach phone numbers to them, and see an incoming call fetch XML from the answer_url and execute verbs (`Play`, `Speak`, `Dial`, `Hangup`, `GetDigits`, `Record`) through the existing IVR runtime
3. Operator can CRUD sub-accounts via `/api/v1/sub-accounts`; every existing api_v1 row defaults to the `root` account and API keys resolve to an `account_id` scope on every request
4. A sub-account Bearer token cannot read or mutate another sub-account's trunk, DID, webhook, or recording — verified by integration test
5. The sub-accounts migration is additive and all pre-existing rows inherit the `root` account_id without data loss

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy](../04-subsystems/)
- [call](../04-subsystems/)
- [rwi](../04-subsystems/)
- [console](../04-subsystems/)
- [models](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/13-cpaas-layer/`
**Last reviewed:** 2026-04-16
