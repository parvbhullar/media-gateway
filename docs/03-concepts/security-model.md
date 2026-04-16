# Security Model

SuperSip enforces security at multiple layers: SIP signalling, API
access, management console, and media transport. This page describes
each layer and how they compose.

## Layer 1: SIP authentication

SIP endpoints must authenticate via **Digest authentication** (RFC 2617)
before placing or receiving calls. The `AuthModule` (`proxy/auth.rs`)
challenges REGISTER and INVITE requests with a 401/407 response
containing a nonce. The endpoint must reply with a valid digest
computed from its credentials.

Credentials are resolved through pluggable `AuthBackend` implementations:

| Backend        | Source                           |
|----------------|----------------------------------|
| `user_plain`   | Static users in TOML config       |
| `user_db`      | Database (`rustpbx_sip_users`)    |
| `user_http`    | External HTTP webhook             |

Authentication errors are typed (`AuthError`) to distinguish between
not-found, disabled, invalid credentials, spam detection, and payment-
required states. Each error type maps to an appropriate SIP response
code.

Trunk-originated calls bypass Digest authentication — they are
validated by IP address in the ACL layer instead.

## Layer 2: ACL (IP-based access control)

The `AclModule` (`proxy/acl.rs`) runs before authentication and
enforces IP-level access policy:

- **CIDR rules** — ordered list of `allow`/`deny` rules with IPv4 and
  IPv6 CIDR support. Rules are evaluated top-to-bottom; first match wins.
- **Default policy** — when no rules are configured, the default is
  `allow all` followed by `deny all` (effectively allow-all).
- **User-Agent filtering** — separate whitelist and blacklist by
  User-Agent header string. Requests with blacklisted UAs are dropped
  before they consume authentication resources.
- **Trunk bypass** — requests from known trunk IPs are automatically
  allowed and tagged with `TrunkContext`.

ACL rules can be loaded from TOML config or from the database via
`ProxyDataContext` and hot-reloaded without restart.

## Layer 3: RBAC (role-based access control)

The console and API enforce role-based permissions via the RBAC model
(`models/rbac.rs`):

### Roles

Roles are stored in `rustpbx_roles` with fields:

| Field         | Description                        |
|---------------|------------------------------------|
| `name`        | Unique role name (e.g. `admin`, `agent`, `viewer`) |
| `description` | Human-readable description          |
| `is_system`   | Whether this is a built-in role     |

### Permissions

Each role has associated permissions in `rustpbx_role_permissions`:

| Field      | Description                          |
|------------|--------------------------------------|
| `resource` | The resource being protected (e.g. `calls`, `trunks`, `users`) |
| `action`   | The permitted action (e.g. `read`, `write`, `delete`, `admin`) |

### User-role assignment

Users are assigned roles via `rustpbx_user_roles`, supporting multiple
roles per user. Permission checks aggregate all permissions across a
user's assigned roles.

## Layer 4: Console authentication

The management console (`console/auth.rs`) uses session-based
authentication:

- **Login** — users authenticate with email/password. Passwords are
  hashed with **Argon2** (via `argon2` crate with `OsRng` salt).
- **Sessions** — signed HMAC-SHA256 cookies with format
  `user_id:expires:signature`. Session TTL is 12 hours.
- **MFA** — optional multi-factor authentication with a separate
  5-minute MFA session cookie.
- **Password reset** — time-limited reset tokens valid for 30 minutes.
- **Registration policy** — configurable: disabled, first-user-only, or
  open registration.

## Layer 5: API authentication

The REST API (`handler/api_v1/auth.rs`) uses **Bearer token**
authentication:

- **API keys** — issued as `rpbx_<64-hex>` strings (69 characters).
  Only the SHA-256 hash is stored in `rustpbx_api_keys`; the plaintext
  is shown exactly once at creation time.
- **Verification** — constant-time comparison (`subtle::ConstantTimeEq`)
  prevents timing attacks.
- **Revocation** — keys can be revoked by setting `revoked_at`;
  revoked keys are rejected immediately.
- **Usage tracking** — `last_used_at` is updated asynchronously on each
  successful authentication.

## Layer 6: TLS and SRTP

SuperSip supports encrypted transport at both the signalling and media
layers:

| Layer       | Protocol       | Configuration                 |
|-------------|----------------|-------------------------------|
| Signalling  | TLS (SIPS)     | `proxy.tls` with cert/key     |
| Signalling  | WSS            | WebSocket over TLS             |
| Media       | SRTP           | DTLS key exchange              |
| Media       | DTLS-SRTP      | WebRTC mandatory encryption    |
| Certificate | ACME           | Auto-renewal via addon         |

The `tls_reloader` module monitors certificate files and hot-reloads
them without dropping active connections. The ACME addon can
automatically provision and renew Let's Encrypt certificates.

## Frequency limiting

The `FrequencyLimiter` trait (`call/policy.rs`) provides rate limiting
for SIP transactions. This prevents registration floods and INVITE
storms from overwhelming the system.

## Roadmap

| Phase | Feature                                    | Status   |
|-------|--------------------------------------------|----------|
| 10    | Runtime firewall (flood/brute-force protection, topology hiding) | Planned |

## Further reading

- [Proxy subsystem](../04-subsystems/proxy.md) — ACL and auth implementation
- [Addons subsystem](../04-subsystems/addons.md) — ACME and security addons
- [Routing Pipeline](routing-pipeline.md) — where ACL and auth fit in the call flow
