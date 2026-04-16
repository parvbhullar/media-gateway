# First Webhook

Route incoming calls dynamically by wiring up the HTTP Router.

## Step 1: Configure the HTTP Router

Add the following to your `config.toml`:

```toml
[proxy.http_router]
url = "http://localhost:5000/route"
fallback_to_static = true
timeout_ms = 3000

[proxy.http_router.headers]
X-API-Key = "my-secret"
```

`fallback_to_static = true` means SuperSip falls back to its internal routing
rules if your webhook is unreachable.

Restart SuperSip to apply.

## Step 2: Create a Webhook Receiver

A minimal Python receiver using Flask:

```python
from flask import Flask, request, jsonify

app = Flask(__name__)

@app.route("/route", methods=["POST"])
def route_call():
    payload = request.get_json()
    print("Incoming call:", payload)

    return jsonify({
        "action": "forward",
        "targets": [f"sip:1001@192.168.1.50:5060"],
        "record": True,
        "timeout": 30,
    })

if __name__ == "__main__":
    app.run(host="0.0.0.0", port=5000)
```

Run it:

```bash
pip install flask
python webhook.py
```

## Step 3: Place a Call

Call any number through SuperSip. On every incoming INVITE, SuperSip sends a
POST to your webhook. Your handler returns a JSON routing decision and the call
is forwarded accordingly.

## Step 4: Observe

- Check your webhook server logs to see the request payload.
- Check **Call Records** in the web console to see the routed call and its CDR.

## Request / Response Format

**POST payload** (sent by SuperSip):

```json
{
  "call_id": "ab39-551-229",
  "from": "<sip:1001@pbx.com>",
  "to": "<sip:200@pbx.com>",
  "source_addr": "1.2.3.4:5060",
  "direction": "inbound",
  "method": "INVITE",
  "uri": "sip:200@pbx.com",
  "headers": { "User-Agent": "Ooh/1.0" },
  "body": "v=0\r\n..."
}
```

**Your response**:

```json
{
  "action": "forward",
  "targets": ["sip:1001@192.168.1.50:5060"],
  "strategy": "parallel",
  "record": true,
  "timeout": 30,
  "media_proxy": "auto"
}
```

Supported actions: `forward`, `reject`, `abort`, `spam`, `not_handled`.

See [HTTP Router Reference](../05-integration/http-router.md) for the full
protocol spec.

## See Also

- [HTTP Router Reference](../05-integration/http-router.md) -- full protocol spec
- [Configuration: Routing](../config/04-routing.md) -- all routing config options

---
**Status:** Shipped
