# RWI Model

**RWI (Real-time WebSocket Integration)** is SuperSip's control-plane
protocol. It lets external applications — AI agents, CRM systems, call
center dashboards — observe and control live calls in real time.

This page describes the conceptual model. For the wire-level protocol
specification, see [RWI Protocol](../05-integration/rwi-protocol.md).

## Architecture

RWI uses a **JSON-over-WebSocket** transport. An application connects to
SuperSip's RWI endpoint, authenticates, and then exchanges two types of
messages:

- **Commands** — sent by the application to SuperSip (e.g. answer a call,
  play audio, transfer).
- **Events** — sent by SuperSip to the application (e.g. call incoming,
  call answered, DTMF received).

```
Application          WebSocket          SuperSip
    |                                       |
    |--- session_subscribe {contexts} ----->|
    |<-- call_incoming {...} ---------------|  (event)
    |--- call_answer {call_id} ------------>|  (command)
    |<-- call_answered {call_id} -----------|  (event)
    |--- media_play {call_id, url} -------->|
    |<-- media_play_finished {call_id} -----|
    |--- call_hangup {call_id} ------------>|
    |<-- call_hangup {call_id} -------------|
```

Every command/event is wrapped in an `RwiEnvelope` with a version field
(`"rwi": "1.0"`) for protocol evolution.

## Command/event correlation

Commands include an `action_id` field that is echoed in the
corresponding response event. This allows applications to correlate
asynchronous results with the commands that triggered them, even when
multiple operations are in flight simultaneously.

## Context-based subscription

Rather than receiving events for all calls on the system, applications
subscribe to **contexts** — named scopes that group related calls:

```json
{"rwi": "1.0", "session_subscribe": {"contexts": ["queue:support", "trunk:main"]}}
```

SuperSip routes events only to sessions subscribed to the relevant
context. This keeps event traffic focused and enables multi-tenant
isolation.

## Session and call ownership

The RWI gateway (`rwi/gateway.rs`) manages the relationship between
WebSocket sessions and calls:

- **Session** — a single WebSocket connection with its own identity,
  subscriptions, and command channel.
- **Call ownership** — a session can *attach* to a call in `Control`
  mode, gaining exclusive command authority. Only the owning session
  can issue call-mutating commands.
- **Supervisor mode** — sessions can attach in `Listen`, `Whisper`, or
  `Barge` mode for call center supervision without taking ownership.

The gateway maintains a cache of recent events so that newly connecting
sessions can replay missed events within a configurable window (default:
1000 events, 60 seconds).

## Command categories

RWI commands group into functional areas:

### Call control
| Command           | Description                        |
|-------------------|------------------------------------|
| `call_originate`  | Place an outbound call              |
| `call_answer`     | Answer an incoming call             |
| `call_reject`     | Reject with reason code             |
| `call_hangup`     | Terminate an active call            |
| `call_bridge`     | Connect two call legs               |
| `call_unbridge`   | Disconnect bridged legs             |
| `call_transfer`   | Blind or attended transfer          |

### Media control
| Command              | Description                     |
|----------------------|---------------------------------|
| `media_play`         | Play audio file into call        |
| `media_stop`         | Stop audio playback              |
| `media_stream_start` | Start real-time audio streaming  |
| `media_stream_stop`  | Stop audio streaming             |
| `media_inject_start` | Inject external audio source     |
| `media_inject_stop`  | Stop audio injection             |

### Recording
| Command          | Description                         |
|------------------|-------------------------------------|
| `record_start`   | Begin call recording                |
| `record_pause`   | Pause active recording              |
| `record_resume`  | Resume paused recording             |
| `record_stop`    | Stop and finalize recording         |

### Queue management
| Command              | Description                      |
|----------------------|----------------------------------|
| `queue_enqueue`      | Place call in a named queue       |
| `queue_dequeue`      | Remove call from queue            |
| `queue_assign_agent` | Assign specific agent to call     |
| `queue_requeue`      | Move call to a different queue    |

### Conference
| Command              | Description                      |
|----------------------|----------------------------------|
| `conference_create`  | Create a conference room          |
| `conference_add`     | Add a call to the conference      |
| `conference_remove`  | Remove a call from the conference |
| `conference_mute`    | Mute a participant                |
| `conference_destroy` | Tear down the conference          |

### Supervisor
| Command              | Description                      |
|----------------------|----------------------------------|
| `supervisor_listen`  | Silent monitoring of a call       |
| `supervisor_whisper` | Speak to agent only               |
| `supervisor_barge`   | Join call as full participant     |

## Smart routing and rule engine

The RWI module includes a **local rule engine** (`rwi/rule_engine.rs`)
that can execute actions without round-tripping to the application:

- **DTMF rules** — match digit patterns and trigger local actions
  (transfer, hold, recording) or forward to the application.
- **Priority routing** — events are routed by priority: Realtime >
  LocalRule > Application. Local rules execute immediately; only
  unhandled events escalate to the WebSocket application.
- **Local actions** — transfer to queue, transfer to number, play
  announcement, start/stop recording, hold/unhold, voicemail, and
  hangup can all execute server-side without application involvement.

This hybrid architecture keeps latency-sensitive operations fast while
still allowing complex logic in external applications.

## Relationship to call sessions

Each RWI-controlled call maps to a `proxy_call/session.rs` CallSession.
The RWI processor translates commands into `SessionAction` enum values
that the session event loop executes. Events flow in the reverse
direction: session state changes emit `ProxyCallEvent` values that the
RWI gateway routes to subscribed sessions.

## Further reading

- [RWI Protocol](../05-integration/rwi-protocol.md) — wire-level specification
- [SIP & B2BUA](sip-and-b2bua.md) — how call sessions work
- [Media Fabric](media-fabric.md) — media control details
