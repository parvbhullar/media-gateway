# First Call

Place a call between two SIP endpoints in under 5 minutes.

## Prerequisites

- SuperSip running (see [Install](install.md))
- A SIP client ([Ooh! SIP](https://ooh.sip), [Linphone](https://www.linphone.org/),
  or the built-in WebRTC phone in the web console)

## Step 1: Configure Users

Add two users to your `config.toml`:

```toml
[[proxy.user_backends]]
type = "memory"
users = [
    { username = "1001", password = "password" },
    { username = "1002", password = "password" },
]
```

Restart SuperSip (or the Docker container) to pick up the change.

## Step 2: Register a Client

Point your SIP client at `udp://localhost:5060` and register as user **1001**.

Alternatively, open the built-in WebRTC phone at
<http://localhost:8080/console/> (log in with your admin credentials first).

## Step 3: Place a Call

Dial `1002` from your SIP client. If 1002 is registered on another device, it
will ring. If 1002 is not registered, the call fails with
**480 Temporarily Unavailable**.

## Step 4: Check the CDR

After hanging up, open the web console and navigate to **Call Records** to see
the CDR entry for your call.

## Next Steps

- [First Webhook](first-webhook.md) -- route calls dynamically via HTTP
- [First RWI Session](first-rwi-session.md) -- control calls in real-time

---
**Status:** Shipped
