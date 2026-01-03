# Kamailio SBC for Voice AI Infrastructure

Multi-tenant SBC/Proxy for bridging private telecom carriers to public Voice AI gateways (LiveKit).

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              PRIVATE NETWORK                                 │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐                   │
│  │   Carrier 1  │    │   Carrier 2  │    │   Carrier N  │                   │
│  │  10.0.1.10   │    │  10.0.1.20   │    │  10.0.1.x    │                   │
│  └──────┬───────┘    └──────┬───────┘    └──────┬───────┘                   │
│         │                   │                   │                            │
│         └───────────────────┼───────────────────┘                            │
│                             │                                                │
│                    ┌────────▼────────┐                                       │
│                    │    KAMAILIO     │                                       │
│                    │   10.0.0.100    │◄──── Private Interface (:5060)       │
│                    │  (SBC/Proxy)    │                                       │
│                    │                 │                                       │
│                    │  ┌───────────┐  │                                       │
│                    │  │ RTPEngine │  │                                       │
│                    │  │  (Media)  │  │                                       │
│                    │  └───────────┘  │                                       │
│                    └────────┬────────┘                                       │
│                             │                                                │
└─────────────────────────────┼────────────────────────────────────────────────┘
                              │
                    ┌─────────▼─────────┐
                    │  203.0.113.50     │◄──── Public Interface (:5080)
                    │  (Public IP)      │
                    └─────────┬─────────┘
                              │
┌─────────────────────────────┼────────────────────────────────────────────────┐
│                             │              PUBLIC NETWORK / CLOUD            │
│         ┌───────────────────┼───────────────────┐                            │
│         │                   │                   │                            │
│  ┌──────▼───────┐    ┌──────▼───────┐    ┌──────▼───────┐                   │
│  │  LiveKit GW  │    │  LiveKit GW  │    │  LiveKit GW  │                   │
│  │  (Group 1)   │    │  (Group 2)   │    │  (Backup)    │                   │
│  │   Default    │    │   Premium    │    │   Group 3    │                   │
│  └──────┬───────┘    └──────┬───────┘    └──────┬───────┘                   │
│         │                   │                   │                            │
│         └───────────────────┼───────────────────┘                            │
│                             │                                                │
│                    ┌────────▼────────┐                                       │
│                    │    LIVEKIT      │                                       │
│                    │   Voice AI      │                                       │
│                    │   Processing    │                                       │
│                    └─────────────────┘                                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Call Flow

### Inbound (Telecom → LiveKit)
```
1. Carrier sends INVITE to Kamailio (10.0.0.100:5060)
2. Kamailio identifies tenant (by source IP, header, or prefix)
3. Kamailio selects gateway from dispatcher (load-balanced)
4. RTPEngine bridges media (internal ↔ external)
5. INVITE forwarded to LiveKit gateway (203.0.113.x:5060)
6. LiveKit processes call with Voice AI
```

### Outbound (LiveKit → Telecom)
```
1. LiveKit sends INVITE to Kamailio (203.0.113.50:5080)
2. Kamailio identifies destination carrier
3. RTPEngine bridges media (external ↔ internal)
4. INVITE forwarded to carrier (10.0.1.x:5060)
5. Telecom delivers call
```

## Quick Start

### 1. Configure IPs

Edit `kamailio.cfg` and update:
```
#!substdef "!PRIVATE_IP!10.0.0.100!g"      # Your private interface
#!substdef "!PUBLIC_IP!203.0.113.50!g"      # Your public interface
#!substdef "!TELECOM_CARRIER_1!10.0.1.10!g" # Primary carrier
#!substdef "!TELECOM_CARRIER_2!10.0.1.20!g" # Secondary carrier
```

Update `rtpengine.conf`:
```
interface = internal/10.0.0.100
interface = external/203.0.113.50
```

### 2. Deploy with Docker

```bash
# Set environment variables
export MYSQL_PASSWORD=your_secure_password

# Start stack
docker-compose up -d

# Wait for MySQL to initialize
sleep 30

# Verify services
docker-compose ps
```

### 3. Add Gateways

```bash
# Using CLI
./kamctl-sbc.sh add-gateway 1 "sip:livekit-gw1.example.com:5060" "LiveKit Primary" 10 50

# Using API
curl -X POST http://localhost:8080/api/v1/gateways \
  -H "Content-Type: application/json" \
  -d '{
    "destination": "sip:livekit-gw1.example.com:5060",
    "group": 1,
    "priority": 10,
    "weight": 50,
    "description": "LiveKit Primary"
  }'
```

### 4. Add Tenants

```bash
# CLI
./kamctl-sbc.sh add-tenant "askiitians" "10.0.1.50" 1 20 200

# API
curl -X POST http://localhost:8080/api/v1/tenants \
  -H "Content-Type: application/json" \
  -d '{
    "tenant_id": "askiitians",
    "source_ip": "10.0.1.50",
    "gateway_group": 1,
    "max_cps": 20,
    "max_concurrent": 200
  }'
```

### 5. Add Carriers

```bash
# Allow carrier IP
./kamctl-sbc.sh add-carrier "10.0.1.10" 32 "Primary-Carrier" 1
```

## Dispatcher Groups

| Group | Purpose | Use Case |
|-------|---------|----------|
| 1 | Default | Standard tenants |
| 2 | Premium | Low-latency, priority routing |
| 3 | Backup | Failover when groups 1/2 fail |

## Tenant Identification

Tenants can be identified by:

1. **Source IP** - Map carrier IP to tenant in `tenant_config`
2. **X-Tenant-ID Header** - Carrier sends header with INVITE
3. **Username Prefix** - Format: `tenantid_phonenumber` (e.g., `mahindra_919876543210`)

## API Endpoints

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/v1/gateways` | List all gateways |
| POST | `/api/v1/gateways` | Add gateway |
| DELETE | `/api/v1/gateways/{id}` | Remove gateway |
| GET | `/api/v1/tenants` | List tenants |
| POST | `/api/v1/tenants` | Add/update tenant |
| DELETE | `/api/v1/tenants/{id}` | Remove tenant |
| POST | `/api/v1/route` | Dynamic routing (for Kamailio) |
| GET | `/api/v1/status` | System status |
| GET | `/api/v1/stats/calls` | Call statistics |

## Performance Tuning

### Kamailio
```bash
# Increase shared memory (256MB default)
kamailio -m 512 -M 128

# Increase children processes
children=16

# Enable async processing
loadmodule "async.so"
```

### RTPEngine
```bash
# Kernel forwarding (requires module)
modprobe xt_RTPENGINE
rtpengine --table=0

# Increase port range
port-min = 10000
port-max = 60000
```

### System
```bash
# File descriptors
ulimit -n 65536

# Network buffers
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216
```

## Monitoring

### Homer (SIP Tracing)
Access at `http://your-server:9080`

### Prometheus Metrics
Kamailio exporter at `:9494/metrics`

### Logs
```bash
# Kamailio logs
tail -f /var/log/kamailio/kamailio.log

# RTPEngine
journalctl -u rtpengine -f
```

## Troubleshooting

### Gateway Not Reachable
```bash
# Check dispatcher status
kamcmd dispatcher.list

# Ping gateway
kamcmd dispatcher.ping 1
```

### Calls Failing
```bash
# Check recent failures
./kamctl-sbc.sh stats

# SIP trace
sngrep -d any port 5060 or port 5080
```

### Media Issues
```bash
# RTPEngine stats
echo '{ "command": "statistics" }' | nc -u 127.0.0.1 2223

# Check interfaces
rtpengine-ctl list numsessions
```

## License

MIT License - Use freely for Unpod.ai Voice AI infrastructure.
