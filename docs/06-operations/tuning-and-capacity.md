# Tuning & Capacity

## Benchmark Results

> Tested on 2026-04-03 · SuperSip 0.4.0 (release) · sipbot 0.2.28 · Linux x86_64 · 16 cores / 32 GB · G.711 PCMU

### Full Comparison

| Level | Scenario | Completion | Peak Conc | Loss | Setup Latency | CPU Peak | Mem Peak |
|-------|----------|:---:|:---:|:---:|:---:|:---:|:---:|
| 500 | mediaproxy=none | 100% | 500 | 0.00% | 4.40ms | 32.4% | 137.3 MB |
| 500 | mediaproxy=all | 100% | 500 | 0.00% | 3.73ms | 98.4% | 183.1 MB |
| 500 | all + sipflow | 100% | 500 | 0.00% | 5.96ms | 101.0% | 198.3 MB |
| 800 | mediaproxy=none | 100% | 800 | 0.00% | 8.32ms | 47.9% | 191.8 MB |
| 800 | mediaproxy=all | 100% | 800 | 0.00% | 6.38ms | 155.0% | 264.8 MB |
| 800 | all + sipflow | 100% | 800 | 0.00% | 6.08ms | 156.0% | 280.3 MB |

### Per-Channel Overhead

| Metric | 500 (none) | 500 (all) | 500 (sipflow) | 800 (none) | 800 (all) | 800 (sipflow) |
|--------|:---:|:---:|:---:|:---:|:---:|:---:|
| CPU (Peak) | 0.065% | 0.197% | 0.202% | 0.060% | 0.194% | 0.195% |
| Memory (Peak) | 0.275 MB | 0.366 MB | 0.397 MB | 0.240 MB | 0.331 MB | 0.350 MB |

### Resource Scaling Estimates

```
mediaproxy=none (signaling only):
  CPU%    ≈ 8 + concurrent × 0.05
  Mem(MB) ≈ 60 + concurrent × 0.16

  1000 conc: CPU ≈ 58% (0.6 cores),  Mem ≈ 220 MB
  2000 conc: CPU ≈ 108% (1.1 cores), Mem ≈ 380 MB
  5000 conc: CPU ≈ 258% (2.6 cores), Mem ≈ 860 MB

mediaproxy=all (RTP forwarding):
  CPU%    ≈ 8 + concurrent × 0.19
  Mem(MB) ≈ 80 + concurrent × 0.23

  1000 conc: CPU ≈ 198% (2.0 cores), Mem ≈ 310 MB
  2000 conc: CPU ≈ 388% (3.9 cores), Mem ≈ 540 MB
  5000 conc: CPU ≈ 958% (9.6 cores), Mem ≈ 1230 MB
```

## Tuning Knobs

### Media Proxy Mode

Configured via `media_proxy` in the `[proxy]` section:

| Mode | Behavior |
|------|----------|
| `none` | Signaling only, lowest CPU |
| `nat` | Relay only when NAT detected |
| `auto` | Relay based on heuristics (default) |
| `all` | Always relay, highest CPU but most reliable |

```toml
[proxy]
media_proxy = "auto"
```

### RTP Port Range

```toml
rtp_start_port = 12000
rtp_end_port = 42000
```

The default range is 20000-30000. Widen the range if you expect high concurrent call counts (each call leg uses one RTP port).

### Database

- **SQLite** (default): WAL mode for concurrent reads. Good for single-instance deployments.
  ```toml
  database_url = "sqlite://rustpbx.sqlite3"
  ```
- **MySQL**: Connection pooling for multi-instance or high-write-volume deployments.
  ```toml
  database_url = "mysql://root@localhost:3306/rustpbx"
  ```

### Logging

- **Production:** `log_level = "info"` — minimal overhead
- **Debug:** `log_level = "debug"` — verbose, impacts performance; use temporarily
- **Log to file:** `log_file = "/var/log/supersip/rustpbx.log"`

### File Descriptors

For high call volumes, raise the open file limit:

```ini
# systemd unit override
LimitNOFILE=65536
```

Or via ulimit before starting the process:

```bash
ulimit -n 65536
```

---
**Status:** ✅ Shipped
**Source:** `README.md` (Benchmark section)
**Last reviewed:** 2026-04-16
