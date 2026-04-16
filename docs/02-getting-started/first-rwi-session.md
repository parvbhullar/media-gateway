# First RWI Session

Control calls in real-time using the WebSocket Interface (RWI).

## Step 1: Configure RWI

Add the following to your `config.toml`:

```toml
[rwi]
enabled = true

[[rwi.tokens]]
token = "my-secret-token"
scopes = ["call.control", "media.stream"]

[[rwi.contexts]]
name = "default"
no_answer_timeout_secs = 30
no_answer_action = "hangup"
```

Restart SuperSip to apply.

## Step 2: Connect

Using [wscat](https://github.com/websockets/wscat):

```bash
wscat -c "ws://localhost:8080/rwi/v1" \
  -H "Authorization: Bearer my-secret-token"
```

If the token is valid you will see the WebSocket connection open. An invalid or
missing token results in HTTP 401 before the handshake completes.

## Step 3: Subscribe to Events

Send a subscribe command to receive inbound call events for the `default`
context:

```json
{
  "action": "session.subscribe",
  "action_id": "sub-001",
  "params": {
    "contexts": ["default"]
  }
}
```

You will receive a `command_completed` event confirming the subscription.

## Step 4: Originate a Call

Initiate an outbound call:

```json
{
  "action": "call.originate",
  "action_id": "orig-001",
  "params": {
    "call_id": "leg_a",
    "destination": "sip:1001@localhost",
    "caller_id": "4000",
    "timeout_secs": 30
  }
}
```

The result arrives as an asynchronous event:

```json
{
  "type": "command_completed",
  "action_id": "orig-001",
  "action": "call.originate",
  "status": "success",
  "data": { "call_id": "leg_a" }
}
```

## Step 5: Observe Events

As the call progresses you will see events such as:

```json
{
  "event": "call.ringing",
  "call_id": "leg_a",
  "data": {}
}
```

```json
{
  "event": "call.answered",
  "call_id": "leg_a",
  "data": {}
}
```

```json
{
  "event": "call.hangup",
  "call_id": "leg_a",
  "data": {}
}
```

All commands use the same async pattern: send an `action` with an `action_id`,
and correlate the response via the matching `action_id` in the
`command_completed` or `command_failed` event.

## See Also

- [RWI Protocol Reference](../05-integration/rwi-protocol.md) -- full command and event reference
- [RWI Concepts](../03-concepts/rwi-model.md) -- how RWI fits into the architecture

---
**Status:** Shipped
