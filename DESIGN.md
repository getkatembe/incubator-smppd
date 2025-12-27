# smppd Design Document

The unified SMPP daemon - gateway, router, load balancer, and proxy in one.

## Overview

**smppd** is a high-performance, configuration-driven SMPP daemon that combines:

- **Gateway** - Accept ESME connections, bridge HTTP↔SMPP
- **Router** - Intelligent message routing with MNP/HLR
- **Load Balancer** - Distribute traffic across SMSC pools
- **Proxy** - Transparent SMPP forwarding

All features emerge from configuration - no mode flags.

### Design Philosophy

1. **Configuration-driven** - Behavior determined by config, not flags
2. **High performance** - 10,000+ msg/sec per node
3. **Zero message loss** - Persistent queuing, guaranteed delivery
4. **Built on smpp-go** - Leverages our SMPP library
5. **Observable** - Prometheus metrics, structured logging
6. **Deployable anywhere** - Binary, Docker, Kubernetes

### Target Performance

| Metric | Target |
|--------|--------|
| Throughput | 10,000+ msg/sec per node |
| Latency | < 10ms p99 |
| Uptime | 99.999% (5 nines) |
| Concurrent Sessions | 10,000+ binds |
| SMSC Connections | 1,000+ per pool |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              smppd                                       │
│                                                                          │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                         Listeners                                │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐         │    │
│  │  │  SMPP    │  │  SMPP    │  │   HTTP   │  │   gRPC   │         │    │
│  │  │  :2775   │  │  :8775   │  │  :8080   │  │  :9090   │         │    │
│  │  │  (plain) │  │  (TLS)   │  │  (REST)  │  │          │         │    │
│  │  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘         │    │
│  └───────┼─────────────┼─────────────┼─────────────┼───────────────┘    │
│          │             │             │             │                     │
│          └─────────────┴──────┬──────┴─────────────┘                     │
│                               ▼                                          │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                      Middleware Chain                            │    │
│  │  Auth → RateLimit → AddressFilter → Transform → Metrics         │    │
│  └─────────────────────────────┬───────────────────────────────────┘    │
│                                ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                      Routing Engine                              │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐         │    │
│  │  │  Prefix  │  │   Cost   │  │   MNP    │  │   Lua    │         │    │
│  │  │  Match   │  │  Based   │  │  Lookup  │  │  Rules   │         │    │
│  │  └──────────┘  └──────────┘  └──────────┘  └──────────┘         │    │
│  └─────────────────────────────┬───────────────────────────────────┘    │
│                                ▼                                         │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                       Upstream Pools                             │    │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │    │
│  │  │  carrier-a  │  │  carrier-b  │  │   backup    │              │    │
│  │  │  ┌───┬───┐  │  │  ┌───┬───┐  │  │  ┌───┐      │              │    │
│  │  │  │ 1 │ 2 │  │  │  │ 1 │ 2 │  │  │  │ 1 │      │              │    │
│  │  │  └───┴───┘  │  │  └───┴───┘  │  │  └───┘      │              │    │
│  │  └─────────────┘  └─────────────┘  └─────────────┘              │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
              ┌──────────┐  ┌──────────┐  ┌──────────┐
              │  SMSC 1  │  │  SMSC 2  │  │  SMSC 3  │
              └──────────┘  └──────────┘  └──────────┘
```

---

## Configuration

All features are enabled through configuration:

```yaml
# /etc/smppd/smppd.yaml

# Listeners - what protocols to accept
listeners:
  - name: smpp
    type: smpp
    address: :2775

  - name: smpp-tls
    type: smpp
    address: :8775
    tls:
      cert: /etc/smppd/certs/server.crt
      key: /etc/smppd/certs/server.key

  - name: http
    type: http
    address: :8080

  - name: grpc
    type: grpc
    address: :9090

# Upstreams - SMSC connection pools
upstreams:
  - name: carrier-a
    hosts:
      - address: smsc1.carrier-a.com:2775
        weight: 100
      - address: smsc2.carrier-a.com:2775
        weight: 100
    bind:
      system_id: ${CARRIER_A_USER}
      password: ${CARRIER_A_PASS}
      type: transceiver
    pool:
      min_connections: 5
      max_connections: 50

  - name: carrier-b
    hosts:
      - address: smsc.carrier-b.com:2775
    bind:
      system_id: ${CARRIER_B_USER}
      password: ${CARRIER_B_PASS}

  - name: backup
    hosts:
      - address: backup.smsc.com:2775
    bind:
      system_id: backup
      password: ${BACKUP_PASS}

# Routes - how to route messages
routes:
  - name: mozambique
    match:
      destination_addr: "+258*"
    upstream: carrier-a

  - name: south-africa
    match:
      destination_addr: "+27*"
    upstream: carrier-b

  - name: default
    match:
      destination_addr: "*"
    upstream: carrier-a
    failover: backup

# Clients - ESME authentication
clients:
  - system_id: client-a
    password: ${CLIENT_A_PASS}
    allowed_ips: ["192.168.1.0/24"]
    rate_limit: 1000

  - system_id: client-b
    password: ${CLIENT_B_PASS}
    rate_limit: 500
```

---

## Listeners

Each listener is independent with its own TLS, timeouts, limits, and settings.

### Multiple Listeners Example

```yaml
listeners:
  # Plain SMPP - internal network
  - name: smpp-internal
    type: smpp
    address: 10.0.0.1:2775
    max_connections: 1000

  # TLS SMPP - external clients, strict mTLS
  - name: smpp-external
    type: smpp
    address: 0.0.0.0:8775
    tls:
      cert: /etc/smppd/certs/external.crt
      key: /etc/smppd/certs/external.key
      ca: /etc/smppd/certs/client-ca.crt
      client_auth: require
    max_connections: 5000

  # HTTP API - internal
  - name: http-internal
    type: http
    address: 10.0.0.1:8080
    timeouts:
      read: 30s
      write: 30s

  # HTTPS API - external with different cert
  - name: https-external
    type: http
    address: 0.0.0.0:8443
    tls:
      cert: /etc/smppd/certs/api.crt
      key: /etc/smppd/certs/api.key
    timeouts:
      read: 60s
      write: 60s

  # gRPC - internal only
  - name: grpc
    type: grpc
    address: 127.0.0.1:9090
```

### Listener Configuration (Full Reference)

```yaml
listeners:
  - name: smpp-main
    type: smpp
    address: :2775

    # TLS/mTLS
    tls:
      enabled: true
      cert: /etc/smppd/certs/server.crt
      key: /etc/smppd/certs/server.key
      ca: /etc/smppd/certs/ca.crt
      client_auth: require       # none, request, require
      min_version: "1.2"
      max_version: "1.3"
      cipher_suites:
        - TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384
        - TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256

    # Connection limits
    max_connections: 10000
    max_connections_per_ip: 100
    max_connections_per_client: 50

    # Timeouts
    timeouts:
      read: 60s
      write: 30s
      idle: 5m
      bind: 30s
      response: 30s

    # SMPP protocol settings
    smpp:
      versions: [3.3, 3.4, 5.0]
      auto_negotiate: true
      enquire_link_interval: 30s
      enquire_link_timeout: 10s
      window_size: 100

    # Rate limiting (per listener)
    rate_limit:
      connections_per_second: 100
      binds_per_second: 50

    # IP filtering
    allowed_ips:
      - 192.168.0.0/16
      - 10.0.0.0/8
    blocked_ips:
      - 10.0.0.99

  - name: http-api
    type: http
    address: :8080

    # TLS
    tls:
      enabled: true
      cert: /etc/smppd/certs/api.crt
      key: /etc/smppd/certs/api.key

    # HTTP settings
    http:
      read_timeout: 30s
      write_timeout: 30s
      idle_timeout: 120s
      max_header_bytes: 1048576
      max_body_bytes: 10485760

    # Rate limiting
    rate_limit:
      requests_per_second: 1000
      burst: 2000

    # CORS
    cors:
      enabled: true
      allowed_origins: ["https://app.example.com"]
      allowed_methods: ["GET", "POST"]
      allowed_headers: ["Authorization", "Content-Type"]

  - name: grpc
    type: grpc
    address: :9090

    tls:
      cert: /etc/smppd/certs/grpc.crt
      key: /etc/smppd/certs/grpc.key

    grpc:
      max_recv_msg_size: 4194304
      max_send_msg_size: 4194304
      keepalive:
        time: 30s
        timeout: 10s
```

#### HTTP API Endpoints

```
POST /v1/messages          - Send message
GET  /v1/messages/{id}     - Get message status
POST /v1/messages/batch    - Send batch
GET  /v1/health            - Health check
GET  /v1/metrics           - Prometheus metrics
```

```json
// POST /v1/messages
{
  "to": "+258841234567",
  "from": "MYAPP",
  "text": "Hello World",
  "registered_delivery": true
}

// Response
{
  "message_id": "msg_abc123",
  "status": "accepted",
  "parts": 1
}
```

### gRPC Listener

For programmatic access:

```yaml
listeners:
  - name: grpc
    type: grpc
    address: :9090
    tls:
      cert: /etc/smppd/certs/server.crt
      key: /etc/smppd/certs/server.key
```

---

## Upstreams

Each upstream is independent with its own credentials, TLS, and settings:

```yaml
upstreams:
  # Carrier A - direct connection, TLS, mTLS client cert
  - name: carrier-a
    hosts:
      - address: smsc1.carrier-a.com:8775
        weight: 100
      - address: smsc2.carrier-a.com:8775
        weight: 100

    bind:
      system_id: ${CARRIER_A_USER}
      password: ${CARRIER_A_PASS}
      system_type: "OTP"
      type: transceiver

    tls:
      enabled: true
      cert: /etc/smppd/certs/carrier-a-client.crt   # mTLS client cert
      key: /etc/smppd/certs/carrier-a-client.key
      ca: /etc/smppd/certs/carrier-a-ca.crt

    pool:
      min_connections: 10
      max_connections: 100

  # Carrier B - plain SMPP, different credentials
  - name: carrier-b
    hosts:
      - address: smsc.carrier-b.com:2775

    bind:
      system_id: ${CARRIER_B_USER}
      password: ${CARRIER_B_PASS}
      system_type: ""
      type: transmitter

    # No TLS
    tls:
      enabled: false

    pool:
      min_connections: 5
      max_connections: 20

  # Aggregator - TLS but no client cert
  - name: aggregator
    hosts:
      - address: smsc.aggregator.com:8775

    bind:
      system_id: ${AGG_USER}
      password: ${AGG_PASS}
      type: transceiver

    tls:
      enabled: true
      skip_verify: false
      # No client cert - server TLS only

  # Backup - different region, different creds
  - name: backup
    hosts:
      - address: backup-eu.smsc.com:2775
      - address: backup-us.smsc.com:2775

    bind:
      system_id: ${BACKUP_USER}
      password: ${BACKUP_PASS}
      type: transceiver
```

### Upstream Configuration (Full Reference)

Each upstream has its own independent configuration:

```yaml
upstreams:
  - name: carrier-a

    # Hosts - multiple for load balancing/failover
    hosts:
      - address: smsc1.carrier-a.com:2775
        weight: 100
        priority: 1
      - address: smsc2.carrier-a.com:2775
        weight: 100
        priority: 1
      - address: smsc3.carrier-a.com:2775
        weight: 50
        priority: 2  # Lower priority = backup

    # Bind credentials
    bind:
      system_id: ${CARRIER_A_USER}
      password: ${CARRIER_A_PASS}
      system_type: "OTP"
      type: transceiver          # transmitter, receiver, transceiver
      version: 3.4               # SMPP version
      interface_version: 0x34
      addr_ton: 0
      addr_npi: 0
      address_range: ""

    # TLS/mTLS
    tls:
      enabled: true
      cert: /etc/smppd/certs/carrier-a-client.crt
      key: /etc/smppd/certs/carrier-a-client.key
      ca: /etc/smppd/certs/carrier-a-ca.crt
      skip_verify: false
      server_name: smsc.carrier-a.com    # SNI
      min_version: "1.2"
      max_version: "1.3"
      cipher_suites:
        - TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384

    # Connection pool
    pool:
      min_connections: 5
      max_connections: 50
      idle_timeout: 5m
      max_lifetime: 1h

    # Timeouts
    timeouts:
      connect: 10s
      bind: 30s
      submit: 30s
      response: 30s
      enquire_link: 60s
      read: 60s
      write: 30s

    # Health checks
    health:
      enabled: true
      type: enquire_link       # tcp, enquire_link, submit
      interval: 30s
      timeout: 10s
      threshold: 3             # Failures before unhealthy
      recovery_threshold: 2    # Successes before healthy

    # Load balancing
    load_balancing:
      algorithm: weighted_round_robin
      # round_robin, least_connections, weighted_round_robin,
      # random, ip_hash, latency

    # Retry
    retry:
      enabled: true
      max_attempts: 3
      delay: 1s
      max_delay: 30s
      backoff: exponential     # fixed, exponential, linear
      backoff_factor: 2
      retryable_errors:
        - 0x00000008           # System error
        - 0x00000058           # Throttled

    # Rate limiting (to protect upstream)
    rate_limit:
      messages_per_second: 500
      burst: 1000

    # Window (max in-flight)
    window:
      size: 100
      timeout: 60s

    # Enquire link
    enquire_link:
      interval: 30s
      timeout: 10s

    # Protocol options
    protocol:
      version: 3.4
      auto_respond_enquire: true
      auto_respond_unbind: true

    # Message options
    message:
      max_length: 160
      default_encoding: gsm7
      default_registered_delivery: 0
      default_service_type: ""
      default_source_addr_ton: 5
      default_source_addr_npi: 0

    # Failover
    failover:
      enabled: true
      threshold: 3             # Consecutive failures
      recovery_time: 60s
      fallback: backup         # Upstream to failover to

    # Tags/metadata
    tags:
      region: eu
      tier: premium
      cost_per_sms: 0.01

---

## Routing

### Route Matching

```yaml
routes:
  # Match by destination prefix
  - name: mozambique-vodacom
    match:
      destination_addr: "+25884*"
    upstream: vodacom-mz

  # Match by destination prefix (country)
  - name: mozambique
    match:
      destination_addr: "+258*"
    upstream: carrier-a

  # Match by source address
  - name: premium-sender
    match:
      source_addr: "PREMIUM"
    upstream: premium-carrier

  # Match by service type
  - name: marketing
    match:
      service_type: "MKTG"
    upstream: marketing-carrier

  # Match by client
  - name: vip-client
    match:
      client: "vip-client-a"
    upstream: premium-carrier

  # Match by time
  - name: night-rates
    match:
      destination_addr: "*"
      schedule:
        timezone: "Africa/Maputo"
        hours: "22:00-06:00"
    upstream: budget-carrier

  # Multiple conditions (AND)
  - name: complex-route
    match:
      destination_addr: "+258*"
      source_addr_ton: 5  # Alphanumeric
      client: "client-a"
    upstream: carrier-a

  # Default route (catch-all)
  - name: default
    match:
      destination_addr: "*"
    upstream: carrier-a
    failover: backup
```

### Cost-Based Routing

```yaml
routes:
  - name: cost-optimized
    match:
      destination_addr: "*"
    upstream: cost-pool

upstreams:
  - name: cost-pool
    load_balancing:
      algorithm: cost_based
    hosts:
      - address: smsc1.carrier-a.com:2775
        cost:
          "+258": 0.010
          "+27": 0.015
          "*": 0.025
      - address: smsc.carrier-b.com:2775
        cost:
          "+258": 0.012
          "+27": 0.010
          "*": 0.020
```

### MNP/HLR Routing

```yaml
mnp:
  enabled: true
  provider: xconnect  # xconnect, hlr_lookup, custom

  xconnect:
    url: https://api.xconnect.io/npq
    api_key: ${XCONNECT_API_KEY}

  cache:
    enabled: true
    ttl: 24h
    max_size: 10000000

routes:
  # Route by operator (from MNP lookup)
  - name: vodacom-direct
    match:
      mnc: "01"  # Requires MNP lookup
    upstream: vodacom-direct

  # Route ported numbers differently
  - name: ported-numbers
    match:
      is_ported: true
    upstream: mnp-aware-carrier
```

### Lua Scripting

```yaml
routes:
  - name: custom-logic
    match:
      destination_addr: "*"
    script: /etc/smppd/routing.lua
```

```lua
-- /etc/smppd/routing.lua
function route(msg, ctx)
    local dest = msg.destination_addr
    local client = ctx.client_id

    -- Premium clients get direct routes
    if ctx.client_tier == "premium" then
        if dest:match("^%+258") then
            return "mz-direct"
        end
    end

    -- Time-based routing
    local hour = os.date("*t").hour
    if hour >= 22 or hour < 6 then
        return "night-rates"
    end

    -- Default
    return "default"
end
```

---

## Clients

ESME authentication and authorization:

```yaml
clients:
  - system_id: client-a
    password: ${CLIENT_A_PASS}

    # IP restrictions
    allowed_ips:
      - 192.168.1.0/24
      - 10.0.0.0/8

    # Bind restrictions
    bind_types: [transmitter, transceiver]
    max_connections: 10

    # Rate limiting
    rate_limit:
      messages_per_second: 1000
      window_size: 5000  # Max in-flight

    # Address restrictions
    addresses:
      source:
        allowed: ["MYAPP", "CLIENT-A"]
      destination:
        blocked: ["+1900*", "+1976*"]  # Block premium

    # Force specific upstream
    upstream: premium-carrier

    # Metadata
    tier: premium
    tags:
      billing_id: "12345"

  - system_id: client-b
    password: ${CLIENT_B_PASS}
    rate_limit:
      messages_per_second: 100
    tier: standard
```

### External Authentication

```yaml
auth:
  # LDAP
  ldap:
    enabled: true
    url: ldap://ldap.example.com:389
    bind_dn: cn=admin,dc=example,dc=com
    bind_password: ${LDAP_PASS}
    base_dn: ou=smpp,dc=example,dc=com

  # RADIUS
  radius:
    enabled: false
    server: radius.example.com:1812
    secret: ${RADIUS_SECRET}

  # HTTP (REST API)
  http:
    enabled: false
    url: https://auth.example.com/smpp/verify
    timeout: 5s
    cache_ttl: 5m
```

---

## Middleware

Processing pipeline:

```yaml
middleware:
  # Logging
  - type: logger
    level: info

  # Authentication (always enabled if clients defined)
  - type: auth

  # Rate limiting
  - type: rate_limit

  # Address filtering
  - type: address_filter

  # Message transformation
  - type: transform
    rules:
      # Rewrite sender
      - match:
          client: "client-a"
        set:
          source_addr: "CLIENT-A"
          source_addr_ton: 5

      # Add country code
      - match:
          destination_addr: "^8[0-9]{8}$"
        transform:
          destination_addr: "+258${destination_addr}"

      # Append opt-out
      - match:
          service_type: "MKTG"
        transform:
          short_message: "${short_message} Reply STOP to opt out."

  # Protocol translation
  - type: protocol
    version_mapping:
      "3.3": "3.4"  # Translate 3.3 clients to 3.4 for SMSC

  # Metrics
  - type: metrics
    prometheus: true
```

---

## Multipart Handling

```yaml
multipart:
  # Reassembly for incoming
  reassembly:
    enabled: true
    timeout: 5m
    max_parts: 20
    buffer_size: 100000

  # Segmentation for outgoing
  segmentation:
    enabled: true
    method: udh  # udh, sar_tlv
    max_parts: 10

  # Reference number
  reference:
    mode: regenerate  # preserve, regenerate, sequential
```

---

## Storage

Local message storage (like smpp-cli daemon):

```yaml
storage:
  enabled: true
  backend: sqlite  # sqlite, badger

  sqlite:
    path: /var/lib/smppd/messages.db
    journal_mode: wal

  # Store received deliver_sm
  deliver_sm:
    enabled: true
    retention: 30d

  # Store submitted messages (for status tracking)
  submit_sm:
    enabled: true
    retention: 7d

  # CDR generation
  cdr:
    enabled: true
    path: /var/log/smppd/cdr
    format: json
    rotation:
      size: 100MB
      time: 1h
```

---

## Observability

### Metrics

```yaml
metrics:
  enabled: true

  prometheus:
    enabled: true
    path: /metrics
    port: 9100
```

Exposed metrics:
- `smppd_connections_active{type,client}`
- `smppd_messages_total{direction,status,upstream,client}`
- `smppd_message_latency_seconds{upstream}`
- `smppd_upstream_health{upstream}`
- `smppd_rate_limit_hits_total{client}`

### Logging

```yaml
logging:
  level: info  # debug, info, warn, error
  format: json  # json, text
  output: stdout  # stdout, file
  file:
    path: /var/log/smppd/smppd.log
    rotation:
      size: 100MB
      max_files: 10
```

### Tracing

```yaml
tracing:
  enabled: true

  otlp:
    endpoint: otel-collector:4317

  jaeger:
    endpoint: jaeger:6831
```

---

## High Availability

### Clustering

```yaml
cluster:
  enabled: true
  node_id: ${HOSTNAME}

  discovery:
    method: kubernetes  # kubernetes, consul, static
    kubernetes:
      namespace: smpp
      service: smppd

  sync:
    sessions: true
    messages: true
    config: true
```

### Failover

```yaml
upstreams:
  - name: carrier-a
    hosts:
      - address: smsc1.carrier-a.com:2775
        priority: 1
      - address: smsc2.carrier-a.com:2775
        priority: 2  # Backup

    failover:
      threshold: 3  # Consecutive failures
      recovery_time: 60s

routes:
  - name: primary
    match:
      destination_addr: "*"
    upstream: carrier-a
    failover: backup  # Fallback upstream
```

---

## Management API

```yaml
management:
  enabled: true
  address: :8081

  # Optional auth
  auth:
    type: basic
    username: admin
    password: ${ADMIN_PASS}
```

Endpoints:
```
GET  /api/status              - Daemon status
GET  /api/clients             - List clients
GET  /api/clients/{id}        - Client details
DELETE /api/clients/{id}/connections  - Disconnect client

GET  /api/upstreams           - List upstreams
GET  /api/upstreams/{name}    - Upstream details
POST /api/upstreams/{name}/suspend
POST /api/upstreams/{name}/resume

GET  /api/routes              - List routes
POST /api/config/reload       - Reload configuration
```

---

## CLI

```bash
# Start daemon
$ smppd --config /etc/smppd/smppd.yaml

# Validate config
$ smppd validate --config /etc/smppd/smppd.yaml

# Show status
$ smppd status

# Client management
$ smppd client list
$ smppd client disconnect client-a

# Upstream management
$ smppd upstream list
$ smppd upstream suspend carrier-a
$ smppd upstream resume carrier-a
$ smppd upstream test carrier-a

# Route management
$ smppd route list
$ smppd route test "+258841234567"  # Show which route matches

# Statistics
$ smppd stats
$ smppd stats --client client-a
$ smppd stats --upstream carrier-a

# CDR
$ smppd cdr export --from 2025-12-01 --to 2025-12-27
```

---

## Deployment

### Binary

```bash
$ curl -sSL https://get.katembe.io/smppd | sh
$ smppd --config /etc/smppd/smppd.yaml
```

### Docker

```yaml
# docker-compose.yaml
services:
  smppd:
    image: getkatembe/smppd:latest
    ports:
      - "2775:2775"
      - "8775:8775"
      - "8080:8080"
    volumes:
      - ./smppd.yaml:/etc/smppd/smppd.yaml
      - ./certs:/etc/smppd/certs
    environment:
      - CARRIER_A_USER=${CARRIER_A_USER}
      - CARRIER_A_PASS=${CARRIER_A_PASS}
```

### Kubernetes

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: smppd
spec:
  replicas: 3
  template:
    spec:
      containers:
        - name: smppd
          image: getkatembe/smppd:latest
          ports:
            - containerPort: 2775
            - containerPort: 8080
          volumeMounts:
            - name: config
              mountPath: /etc/smppd
          readinessProbe:
            httpGet:
              path: /health
              port: 8080
```

### Systemd

```ini
# /etc/systemd/system/smppd.service
[Unit]
Description=SMPP Daemon
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/smppd --config /etc/smppd/smppd.yaml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

---

## Package Structure

```
smppd/
├── cmd/
│   └── smppd/
│       └── main.go
├── internal/
│   ├── server/
│   │   ├── server.go
│   │   └── listener.go
│   ├── listener/
│   │   ├── smpp.go
│   │   ├── http.go
│   │   └── grpc.go
│   ├── upstream/
│   │   ├── pool.go
│   │   ├── connection.go
│   │   ├── health.go
│   │   └── loadbalancer.go
│   ├── router/
│   │   ├── engine.go
│   │   ├── matcher.go
│   │   ├── cost.go
│   │   ├── mnp.go
│   │   └── lua.go
│   ├── middleware/
│   │   ├── chain.go
│   │   ├── auth.go
│   │   ├── rate_limit.go
│   │   ├── address_filter.go
│   │   ├── transform.go
│   │   └── metrics.go
│   ├── client/
│   │   ├── registry.go
│   │   └── auth.go
│   ├── storage/
│   │   ├── storage.go
│   │   ├── sqlite.go
│   │   └── cdr.go
│   ├── cluster/
│   │   ├── cluster.go
│   │   └── sync.go
│   ├── api/
│   │   ├── server.go
│   │   └── handlers.go
│   └── config/
│       ├── config.go
│       └── loader.go
├── api/
│   └── smppd/
│       └── v1/
│           └── smppd.proto
└── configs/
    ├── minimal.yaml
    ├── gateway.yaml
    ├── router.yaml
    └── full.yaml
```

---

## Example Configurations

### Minimal (Proxy)

```yaml
# Simplest: SMPP proxy
listeners:
  - type: smpp
    address: :2775

upstreams:
  - name: smsc
    hosts:
      - address: smsc.provider.com:2775
    bind:
      system_id: myuser
      password: mypass

routes:
  - match: { destination_addr: "*" }
    upstream: smsc
```

### Gateway (HTTP → SMPP)

```yaml
# HTTP API to SMPP
listeners:
  - type: http
    address: :8080

upstreams:
  - name: smsc
    hosts:
      - address: smsc.provider.com:2775
    bind:
      system_id: myuser
      password: mypass

routes:
  - match: { destination_addr: "*" }
    upstream: smsc

clients:
  - system_id: api-user
    password: api-pass
```

### Load Balancer

```yaml
# Multiple SMSCs with load balancing
listeners:
  - type: smpp
    address: :2775

upstreams:
  - name: pool
    hosts:
      - address: smsc1.provider.com:2775
        weight: 100
      - address: smsc2.provider.com:2775
        weight: 100
      - address: smsc3.provider.com:2775
        weight: 50
    bind:
      system_id: myuser
      password: mypass
    load_balancing:
      algorithm: weighted_round_robin

routes:
  - match: { destination_addr: "*" }
    upstream: pool
```

### Full Router

```yaml
# Multi-carrier routing
listeners:
  - type: smpp
    address: :2775
  - type: http
    address: :8080

upstreams:
  - name: carrier-a
    hosts:
      - address: smsc.carrier-a.com:2775
    bind:
      system_id: ${CARRIER_A_USER}
      password: ${CARRIER_A_PASS}

  - name: carrier-b
    hosts:
      - address: smsc.carrier-b.com:2775
    bind:
      system_id: ${CARRIER_B_USER}
      password: ${CARRIER_B_PASS}

  - name: backup
    hosts:
      - address: backup.smsc.com:2775
    bind:
      system_id: backup
      password: ${BACKUP_PASS}

routes:
  - name: mozambique
    match: { destination_addr: "+258*" }
    upstream: carrier-a

  - name: south-africa
    match: { destination_addr: "+27*" }
    upstream: carrier-b

  - name: default
    match: { destination_addr: "*" }
    upstream: carrier-a
    failover: backup

clients:
  - system_id: client-a
    password: ${CLIENT_A_PASS}
    rate_limit: { messages_per_second: 1000 }

  - system_id: client-b
    password: ${CLIENT_B_PASS}
    rate_limit: { messages_per_second: 100 }
```

---

## Feature Comparison

| Feature | Melrose Labs (4 products) | smppd (1 product) |
|---------|---------------------------|-------------------|
| SMPP Gateway | ✓ | ✓ |
| SMPP Router | ✓ | ✓ |
| SMPP Load Balancer | ✓ | ✓ |
| SMPP Proxy | ✓ | ✓ |
| HTTP API | ✓ | ✓ |
| gRPC API | ? | ✓ |
| Cost-based Routing | ✓ | ✓ |
| MNP/HLR Lookup | ✓ | ✓ |
| Lua Scripting | ? | ✓ |
| TLS/mTLS | ✓ | ✓ |
| Rate Limiting | ✓ | ✓ |
| Failover | ✓ | ✓ |
| Clustering | ✓ | ✓ |
| Prometheus Metrics | ✓ | ✓ |
| Configuration-driven | ? | ✓ |
| Open Source | ✗ | ✓ (Apache 2.0) |

---

## License

Apache 2.0
