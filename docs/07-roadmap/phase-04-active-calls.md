# Phase 4: Active Calls & Mid-Call Control

## Goal

Expose the active call registry and dispatch mid-call REST commands through the existing `proxy_call/session.rs` path.

## Dependencies

Phase 2.

## Requirements

- **CALL-01**: Operator can list active calls via `GET /api/v1/calls` with pagination
- **CALL-02**: Operator can retrieve a single active call by id
- **CALL-03**: Operator can hangup an active call
- **CALL-04**: Operator can transfer an active call (attended and blind)
- **CALL-05**: Operator can mute and unmute a call leg
- **CALL-06**: `POST /api/v1/calls/{id}/play` plays an audio file to the call
- **CALL-07**: `POST /api/v1/calls/{id}/speak` synthesizes TTS and plays it to the call
- **CALL-08**: `POST /api/v1/calls/{id}/dtmf` transmits touch-tone digits
- **CALL-09**: `POST /api/v1/calls/{id}/record` starts recording with format (mp3/wav) + optional transcription
- **CALL-10**: Mid-call operations dispatch through the existing `active_call_registry` and `proxy_call/session.rs` path

## Success Criteria

1. Operator can list active calls with pagination and retrieve a single call by id
2. Operator can hangup, transfer (attended and blind), and mute/unmute an active call leg
3. Operator can inject `play`, `speak`, `dtmf`, and `record` commands into a live call and observe them land via the active call registry
4. All mid-call operations dispatch through the existing `active_call_registry` → `proxy_call/session.rs` path with no new proxy modules

## Affected Subsystems

- [handler](../04-subsystems/)
- [proxy](../04-subsystems/)
- [call](../04-subsystems/)
- [rwi](../04-subsystems/)

## Plans

Plans not yet created.

---
**Status:** 📋 Planned
**Planning artifacts:** `.planning/phases/04-active-calls-mid-call-control/`
**Last reviewed:** 2026-04-16
