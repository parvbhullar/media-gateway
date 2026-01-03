# Kamailio SBC for Voice AI Infrastructure

Multi-tenant SBC/Proxy for bridging private telecom carriers to public Voice AI gateways (VAPI/LiveKit).

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [File Structure](#file-structure)
- [Setup Instructions](#setup-instructions)
- [Current Deployment](#current-deployment)
- [Gateway Management](#gateway-management)
- [Service Management](#service-management)
- [Troubleshooting](#troubleshooting)

## Overview

This Kamailio SBC provides bidirectional SIP proxy functionality:

| Direction | Flow | Use Case |
|-----------|------|----------|
| **Inbound** | Telecom → Voice AI | Customer calls routed to AI agents |
| **Outbound** | Voice AI → Telecom | AI-initiated calls to customers |

## Architecture

```
┌────────────────────┐                                        ┌────────────────────┐
│   PRIVATE SIDE     │                                        │    PUBLIC SIDE     │
│                    │                                        │                    │
│  ┌──────────────┐  │       ┌──────────────────────┐         │  ┌──────────────┐  │
│  │   Vodafone   │  │       │    KAMAILIO SBC      │         │  │     VAPI     │  │
│  │              │  │       │                      │         │  │  (Group 1)   │  │
│  │ 10.230.73.220│◄─┼──────►│ Private Interface    │         │  └──────────────┘  │
│  │    :5060     │  │  SIP  │ 10.230.73.225:5060   │         │                    │
│  └──────────────┘  │       │                      │         │  ┌──────────────┐  │
│                    │       │  ┌────────────────┐  │         │  │   LiveKit    │  │
│                    │       │  │   RTPEngine    │  │   SIP   │  │  (Group 2)   │  │
│                    │       │  │  127.0.0.1:2223│  │◄───────►│  │  - Primary   │  │
│                    │       │  │  (Media Bridge)│  │         │  │  - DigiPanda │  │
│                    │       │  └────────────────┘  │         │  │  - Voxket    │  │
│                    │       │                      │         │  │  - Fabriq    │  │
│                    │       │ Public Interface     │         │  └──────────────┘  │
│                    │       │ 103.146.242.234:5060 │         │                    │
│                    │       └──────────────────────┘         │  ┌──────────────┐  │
│                    │                                        │  │ Unpod Cloud  │  │
│                    │                                        │  │  (Group 3)   │  │
│                    │                                        │  │   Backup     │  │
│                    │                                        │  └──────────────┘  │
└────────────────────┘                                        └────────────────────┘
```

### Call Flow

```
═══════════════════════════════════════════════════════════════════════════════════
                           INBOUND (Telecom → Voice AI)
═══════════════════════════════════════════════════════════════════════════════════

  Vodafone                 Kamailio SBC                      Voice AI Gateway
  10.230.73.220           10.230.73.225                     (VAPI/LiveKit)
       │                        │                                  │
       │──── INVITE ───────────►│ (Private :5060)                  │
       │                        │                                  │
       │                        │──────── INVITE ─────────────────►│ (Public :5060)
       │                        │         (via Dispatcher)         │
       │                        │                                  │
       │◄─── 200 OK ────────────│◄──────── 200 OK ─────────────────│
       │                        │                                  │
       │════ RTP Media ════════►│◄════════ RTP Media ══════════════│
       │                   (RTPEngine bridges media)               │


═══════════════════════════════════════════════════════════════════════════════════
                           OUTBOUND (Voice AI → Telecom)
═══════════════════════════════════════════════════════════════════════════════════

  Vodafone                 Kamailio SBC                      Voice AI Gateway
  10.230.73.220           10.230.73.225                     (VAPI/LiveKit)
       │                        │                                  │
       │                        │◄─────── INVITE ──────────────────│ (Public :5060)
       │                        │                                  │
       │◄─── INVITE ────────────│ (Private :5060)                  │
       │                        │                                  │
       │──── 200 OK ───────────►│──────── 200 OK ─────────────────►│
       │                        │                                  │
       │◄═══ RTP Media ═════════│═════════ RTP Media ══════════════│
       │                   (RTPEngine bridges media)               │

```

## Prerequisites

- Docker and Docker Compose
- Two network interfaces (private and public)
- Open ports: 5060/udp, 5060/tcp, 10000-20000/udp (RTP)

## File Structure

```
kamailio/
├── docker-compose.yml      # Docker stack definition
├── Dockerfile              # Custom Kamailio image
├── kamailio.cfg            # Main Kamailio configuration
├── kamailio-local.cfg      # Local IP overrides
├── kamailio_schema.sql     # Database schema
├── rtpengine.conf          # RTPEngine configuration
└── README.md               # This file
```

## Setup Instructions

### Step 1: Install Docker

```bash
curl -fsSL https://get.docker.com | sh
```

### Step 2: Configure Network IPs

Edit `kamailio-local.cfg` with your network IPs:

```bash
# kamailio-local.cfg
#!substdef "!PRIVATE_IP!10.230.73.225!g"      # Your private interface
#!substdef "!PUBLIC_IP!103.146.242.234!g"      # Your public interface
#!substdef "!TELECOM_CARRIER_1!10.230.73.220!g" # Telecom carrier IP
```

Update `kamailio.cfg` with the same IPs:

```kamailio
# Network Configuration (around line 25)
#!substdef "!PRIVATE_IP!10.230.73.225!g"
#!substdef "!PUBLIC_IP!103.146.242.234!g"
#!substdef "!PRIVATE_PORT!5060!g"
#!substdef "!PUBLIC_PORT!5060!g"

# Carrier Configuration
#!substdef "!TELECOM_CARRIER_1!10.230.73.220!g"
```

Add aliases for OPTIONS keepalive responses (around line 38):

```kamailio
# Aliases so "myself" matches our listening IPs
alias=10.230.73.225
alias=103.146.242.234
```

### Step 3: Configure RTPEngine

Edit `rtpengine.conf`:

```ini
[rtpengine]
interface = internal/10.230.73.225
interface = external/103.146.242.234
listen-ng = 127.0.0.1:2223
port-min = 10000
port-max = 20000
log-level = 6
foreground = true
```

### Step 4: Update Docker Compose

Ensure `docker-compose.yml` has correct interface IPs for RTPEngine:

```yaml
rtpengine:
  command: >
    rtpengine --config-file=/etc/rtpengine/rtpengine.conf
    --interface=internal/10.230.73.225
    --interface=external/103.146.242.234
    --listen-ng=127.0.0.1:2223
    --port-min=10000
    --port-max=20000
```

### Step 5: Create Log Directories

```bash
sudo mkdir -p /var/log/kamailio /run/kamailio
sudo chmod 777 /var/log/kamailio /run/kamailio
```

### Step 6: Start the Stack

```bash
cd /var/apps/media-gateway/kamailio

# Start all services
docker compose up -d

# Wait for MySQL to initialize
sleep 30

# Verify services
docker compose ps
```

### Step 7: Verify Setup

```bash
# Check Kamailio is listening
ss -lnup | grep ":5060"

# Test OPTIONS response on both interfaces
sipsak -s sip:10.230.73.225:5060   # Private
sipsak -s sip:103.146.242.234:5060  # Public

# Check logs
docker logs kamailio-sbc 2>&1 | head -50
```

## Current Deployment

### Services

| Service | Container | Endpoint |
|---------|-----------|----------|
| Kamailio SBC | kamailio-sbc | 10.230.73.225:5060 (private), 103.146.242.234:5060 (public) |
| RTPEngine | rtpengine | 127.0.0.1:2223 |
| MySQL | kamailio-db | 127.0.0.1:3306 |

### Gateway Groups

| Group | Gateway | Endpoint | Description |
|-------|---------|----------|-------------|
| 1 | VAPI | 66e592d6-d690-45bb-8996-18215af542d0.sip.vapi.ai | Primary AI Gateway |
| 2 | LiveKit | 3i5bvr312d9.sip.livekit.cloud | LiveKit Primary |
| 2 | DigiPanda | 61xh9s3ubwq.sip.livekit.cloud | DigiPanda LiveKit |
| 2 | Voxket | 4gv2kcqpg2d.sip.livekit.cloud | Voxket LiveKit |
| 2 | Fabriq | 15j2dl095m2.sip.livekit.cloud | Fabriq LiveKit |
| 3 | Unpod Cloud | sip-up-tt.unpod.tv | Backup Gateway |
| 3 | Unpod Cloud | sip.unpod.tel | Backup Gateway |

### Network Configuration

- **Private Interface**: `10.230.73.225:5060` - Receives calls from Vodafone
- **Public Interface**: `103.146.242.234:5060` - Connects to Voice AI gateways
- **Telecom Carrier**: Vodafone INT Phony at `10.230.73.220:5060`

## Gateway Management

### List Gateways

```bash
docker exec kamailio-db mysql -u kamailio -pkamailiorw kamailio \
  -e "SELECT setid AS 'Group', destination AS 'Gateway', description FROM dispatcher;"
```

### Add Gateway

```bash
docker exec kamailio-db mysql -u kamailio -pkamailiorw kamailio -e "
INSERT INTO dispatcher (setid, destination, flags, priority, attrs, description) VALUES
(2, 'sip:new-gateway.example.com:5060;transport=tcp', 0, 10, 'weight=50;socket=udp:103.146.242.234:5080', 'New Gateway');
"

# Reload dispatcher
docker exec kamailio-sbc kamcmd dispatcher.reload
```

### Remove Gateway

```bash
docker exec kamailio-db mysql -u kamailio -pkamailiorw kamailio \
  -e "DELETE FROM dispatcher WHERE id = <gateway_id>;"

# Reload dispatcher
docker exec kamailio-sbc kamcmd dispatcher.reload
```

### Reload Dispatcher (after changes)

```bash
docker exec kamailio-sbc kamcmd dispatcher.reload
```

## Database Tables

### dispatcher (Gateway routing)

| Column | Description |
|--------|-------------|
| setid | Gateway group (1=VAPI, 2=LiveKit, 3=Backup) |
| destination | SIP URI of gateway |
| priority | Higher = preferred |
| attrs | weight, socket binding |

### address (IP ACL)

| Column | Description |
|--------|-------------|
| grp | 1=Telecom carriers, 2=Public gateways |
| ip_addr | IP address to allow |
| mask | CIDR mask |
| tag | Description |

### did_routing (DID to Gateway mapping)

| Column | Description |
|--------|-------------|
| did | Phone number (DID) |
| gateway_group | Target dispatcher group |
| gateway_uri | Direct gateway URI (optional) |

## Service Management

### Start Services

```bash
cd /var/apps/media-gateway/kamailio

# Start all services in background
docker compose up -d

# Start with logs visible
docker compose up

# Start and rebuild containers
docker compose up -d --build
```

### Stop Services

```bash
# Stop all services
docker compose down

# Stop without removing containers
docker compose stop
```

### Restart Services

```bash
# Restart all services
docker compose restart

# Restart specific service
docker compose restart kamailio
docker compose restart rtpengine
docker compose restart mysql
```

### View Logs

```bash
# All services
docker compose logs -f

# Specific service
docker logs kamailio-sbc -f
docker logs rtpengine -f
docker logs kamailio-db -f

# Last 50 lines
docker logs kamailio-sbc 2>&1 | tail -50

# Recent logs (last 5 minutes)
docker logs kamailio-sbc --since 5m
```

### Service Status

```bash
# Check running containers
docker compose ps

# Check listening ports
ss -lnup | grep -E "5060|5080"

# Test SIP connectivity
sipsak -s sip:10.230.73.225:5060   # Private interface
sipsak -s sip:103.146.242.234:5060  # Public interface
sipsak -s sip:10.230.73.220:5060    # Vodafone
```

### Kamailio Commands

```bash
# Reload dispatcher (after gateway changes)
docker exec kamailio-sbc kamcmd dispatcher.reload

# List dispatcher gateways
docker exec kamailio-sbc kamcmd dispatcher.list

# View active dialogs/calls
docker exec kamailio-sbc kamcmd dlg.list

# Check RTPEngine connection
docker exec kamailio-sbc kamcmd rtpengine.show all
```

## Troubleshooting

### Check Service Status

```bash
docker compose ps
docker logs kamailio-sbc 2>&1 | tail -50
```

### Test SIP Connectivity

```bash
# Test OPTIONS response
sipsak -s sip:10.230.73.225:5060

# Trace SIP traffic
sngrep -d any port 5060
```

### Common Issues

#### OPTIONS Keepalive Not Responding

Ensure aliases are configured in `kamailio.cfg`:
```kamailio
alias=10.230.73.225
alias=103.146.242.234
```

And OPTIONS handler is FIRST in REQINIT route:
```kamailio
route[REQINIT] {
    # OPTIONS keepalive reply - handle FIRST
    if (is_method("OPTIONS") && uri == myself) {
        sl_send_reply("200", "OK");
        exit;
    }
    # ... rest of checks
}
```

#### Database Connection Errors

Check MySQL is accessible:
```bash
docker exec kamailio-db mysql -u kamailio -pkamailiorw -e "SELECT 1;"
```

Verify db_url in kamailio.cfg uses port:
```kamailio
modparam("dispatcher", "db_url", "mysql://kamailio:kamailiorw@127.0.0.1:3306/kamailio")
```

#### RTPEngine Not Working

Check RTPEngine is running:
```bash
echo '{ "command": "ping" }' | nc -u 127.0.0.1 2223
```

### View Active Calls

```bash
docker exec kamailio-sbc kamcmd dlg.list
```

### Restart Services

```bash
# Restart all
docker compose restart

# Restart just Kamailio
docker compose restart kamailio

# Full rebuild
docker compose down
docker compose up -d --build
```

## Configuration Reference

### Key kamailio.cfg Sections

| Line Range | Section | Purpose |
|------------|---------|---------|
| 25-40 | Network Config | IP addresses, ports, aliases |
| 70-100 | Module Loading | Required Kamailio modules |
| 130-170 | Module Params | Database URLs, dispatcher settings |
| 230-262 | REQINIT | Request validation, OPTIONS handling |
| 300-330 | FROM_TELECOM | Identify calls from carriers |
| 315-328 | FROM_PUBLIC_GATEWAY | Identify calls from AI gateways |
| 331-390 | TO_PUBLIC_GATEWAY | Route to AI gateways (dispatcher) |
| 393-405 | TO_TELECOM | Route to Vodafone carrier |

## License

MIT License - Use freely for Unpod.ai Voice AI infrastructure.
