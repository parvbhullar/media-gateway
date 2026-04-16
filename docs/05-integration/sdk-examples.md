# SDK Examples

Quick-start code snippets for integrating with SuperSip.

---

## curl -- System Health

```bash
curl -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  http://localhost:8080/api/v1/system/health
```

Response:

```json
{"uptime_secs": 86400, "db_ok": true, "active_calls": 5, "version": "0.1.0"}
```

---

## curl -- List Gateways

```bash
curl -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  http://localhost:8080/api/v1/gateways
```

---

## curl -- List DIDs (Filtered)

```bash
curl -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  "http://localhost:8080/api/v1/dids?trunk=carrier-west&page=1&page_size=10"
```

---

## curl -- Create a DID

```bash
curl -X POST \
  -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "number": "+14155551234",
    "trunk_name": "carrier-west",
    "extension_number": "1001",
    "label": "Main Office",
    "enabled": true
  }' \
  http://localhost:8080/api/v1/dids
```

---

## curl -- Create a Trunk Group

```bash
curl -X POST \
  -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "us-east",
    "distribution_mode": "round_robin",
    "members": [
      {"gateway_name": "carrier-west", "weight": 100},
      {"gateway_name": "carrier-east", "weight": 50}
    ]
  }' \
  http://localhost:8080/api/v1/trunks
```

---

## curl -- Hot Reload

```bash
curl -X POST \
  -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  http://localhost:8080/api/v1/system/reload
```

---

## curl -- Probe a Gateway

```bash
curl -X POST \
  -H "Authorization: Bearer rpbx_YOUR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name": "carrier-west"}' \
  http://localhost:8080/api/v1/diagnostics/trunk-test
```

---

## Python -- HTTP Router Webhook Handler

Minimal Flask handler that receives SuperSip routing requests and returns a forwarding decision.

```python
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route("/pbx/route", methods=["POST"])
def handle_route():
    data = request.json
    caller = data.get("from", "")
    destination = data.get("to", "")

    # Route support calls to the IVR, everything else to default
    if destination.endswith("@support.local"):
        targets = ["sip:ivr@192.168.1.10:5060"]
    else:
        return jsonify({"action": "not_handled"})

    return jsonify({
        "action": "forward",
        "targets": targets,
        "strategy": "parallel",
        "timeout": 30,
        "record": True,
    })

if __name__ == "__main__":
    app.run(port=5000)
```

---

## JavaScript -- RWI WebSocket Connection

Connect to the SuperSip RWI WebSocket for real-time call control.

```javascript
// Using wscat: wscat -c ws://localhost:8080/rwi/v1?token=YOUR_AMI_TOKEN

const ws = new WebSocket("ws://localhost:8080/rwi/v1?token=YOUR_AMI_TOKEN");

ws.onopen = () => {
  console.log("Connected to RWI");
  // Subscribe to call events
  ws.send(JSON.stringify({
    action_id: "sub-001",
    action: "event.subscribe",
    params: { events: ["call.*"] }
  }));
};

ws.onmessage = (event) => {
  const msg = JSON.parse(event.data);
  if (msg.type === "call.ringing") {
    console.log(`Call ${msg.call_id} ringing: ${msg.from} -> ${msg.to}`);
  }
};
```

---

## Python -- CDR Webhook Receiver

Receive CDR push events from SuperSip's `[callrecord]` HTTP hook.

```python
from flask import Flask, request

app = Flask(__name__)

@app.route("/pbx/cdr", methods=["POST"])
def handle_cdr():
    cdr_json = request.form.get("calllog.json")
    audio_file = request.files.get("media_audio-0")

    if cdr_json:
        print(f"CDR received: {cdr_json[:200]}")
    if audio_file:
        audio_file.save(f"/recordings/{audio_file.filename}")

    return "", 200

if __name__ == "__main__":
    app.run(port=5001)
```

---

## See Also

- [Carrier API](carrier-api.md) -- full endpoint reference
- [HTTP Router](http-router.md) -- webhook request/response schema
- [RWI Protocol](rwi-protocol.md) -- WebSocket protocol reference

---
**Status:** Shipped
**Last reviewed:** 2026-04-16
