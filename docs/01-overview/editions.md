# Editions

SuperSip ships in two editions that share the same core engine.

## Feature Matrix

| Feature | Community | Commerce |
|---------|:---------:|:--------:|
| SIP Proxy + Media | Yes | Yes |
| HTTP Router (dynamic routing) | Yes | Yes |
| Queue / ACD | Yes | Yes |
| Call Recording + SipFlow | Yes | Yes |
| Transcript (SenseVoice offline) | Yes | Yes |
| Web Console | Yes | Yes |
| RWI (WebSocket Interface) | Yes | Yes |
| **VoIP Wholesale** (VOS3000 alternative) | -- | Yes |
| **IVR Visual Editor** | -- | Planned |
| **Voicemail Pro** | -- | Planned |
| **Enterprise Auth** (LDAP/SAML/MFA) | -- | Planned |
| **Endpoint Manager** (phone auto-provisioning) | -- | Planned |

## Licensing

| Edition | License | Contact |
|---------|---------|---------|
| Community | MIT | [GitHub](https://github.com/restsend/rustpbx) |
| Commerce | Commercial | [hi@miuda.ai](mailto:hi@miuda.ai) |

## Docker Images

```bash
# Commerce (includes Wholesale + all commercial plugins)
docker pull docker.cnb.cool/miuda.ai/rustpbx:latest

# Community
docker pull ghcr.io/restsend/rustpbx:latest
```

---
**Status:** Shipped
**Source:** `README.md`
**Last reviewed:** 2026-04-16
