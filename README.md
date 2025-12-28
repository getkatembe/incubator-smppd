# smppd

High-performance SMPP proxy and router written in Rust.

## Features

### Core

- **SMPP 3.4 Protocol Support** - Full client and upstream connection handling
- **Multi-Cluster Routing** - Route messages based on destination, source, sender ID
- **Load Balancing** - Round-robin, weighted, least-connections, random, P2C strategies
- **Connection Pooling** - Efficient connection reuse to upstream SMSCs
- **TLS Support** - Secure connections with rustls

### Message Handling

- **Message Store** - In-memory storage with configurable expiration
- **Delivery Receipts (DLR)** - Parse, correlate, and forward DLRs to clients
- **Retry Processor** - Automatic retry with exponential backoff
- **Feedback Loop** - Track message status and routing decisions

### Reliability

- **Circuit Breaker** - Automatic endpoint failure detection
- **Health Checks** - Active health monitoring of upstream endpoints
- **Graceful Shutdown** - Connection draining with configurable timeout
- **Hot Reload** - SIGHUP-triggered configuration reload

### Operations

- **Admin API** - HTTP endpoints for health, metrics, and management
- **Prometheus Metrics** - OpenTelemetry-based metrics export
- **Structured Logging** - JSON logging with tracing spans
- **Kubernetes Ready** - Liveness and readiness probes

## Architecture

```
src/
├── admin/          # HTTP admin API (health, metrics, stats)
├── bootstrap/      # Server initialization, shutdown, workers
├── cluster/        # Upstream SMSC clusters and load balancing
├── config/         # Configuration loading (YAML/JSON/TOML)
├── feedback/       # Message feedback and routing decisions
├── filter/         # Request filters (auth, rate-limit, firewall)
├── forwarder/      # Message forwarding, retries, DLR handling
├── listener/       # TCP/TLS connection acceptor
├── router/         # Message routing logic
├── store/          # Message persistence
└── telemetry/      # Metrics and tracing
```

## Quick Start

```bash
# Build
cargo build --release

# Run with config
./target/release/smppd --config config.yaml
```

## Configuration

```yaml
listeners:
  - name: default
    address: "0.0.0.0:2775"
    protocol: smpp34
    limits:
      max_connections: 1000
      idle_timeout: 60s
      request_timeout: 30s

clusters:
  - name: vodacom
    endpoints:
      - address: "smsc.vodacom.mz:2775"
        system_id: myid
        password: secret
    load_balancer: round_robin
    circuit_breaker:
      threshold: 5
      timeout: 30s
    health_check:
      interval: 30s
      timeout: 5s

routes:
  - name: vodacom-route
    match:
      destination:
        prefix: "+25884"
    cluster: vodacom
    priority: 1

admin:
  address: "0.0.0.0:9090"
```

## Admin API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/healthz` | GET | Full health status |
| `/livez` | GET | Liveness probe (Kubernetes) |
| `/readyz` | GET | Readiness probe (Kubernetes) |
| `/metrics` | GET | Prometheus metrics |
| `/stats` | GET | Server statistics |
| `/config` | GET | Current configuration |
| `/config/reload` | POST | Reload configuration |
| `/store/stats` | GET | Message store statistics |
| `/feedback/stats` | GET | Feedback loop statistics |

## Delivery Receipt (DLR) Handling

smppd automatically handles delivery receipts:

1. **Parse** - Extracts message ID, status, and error codes from DLRs
2. **Correlate** - Matches DLRs to stored messages via SMSC message ID
3. **Update** - Updates message state (Delivered/Failed) in store
4. **Forward** - Routes DLRs back to original client sessions

Supported DLR formats:
- Standard text format: `id:XXX sub:001 dlvrd:001 stat:DELIVRD err:000 text:...`
- TLV-based: `receipted_message_id` (0x001E) and `message_state` (0x0427)

## Metrics

Key metrics exposed at `/metrics`:

```
smppd_messages_submitted_total
smppd_messages_delivered_total
smppd_messages_failed_total
smppd_dlr_received_total{cluster, status}
smppd_connection_active{listener}
smppd_endpoint_requests_total{cluster, endpoint}
smppd_endpoint_latency_seconds{cluster, endpoint}
```

## Signals

| Signal | Action |
|--------|--------|
| SIGINT/SIGTERM | Graceful shutdown with connection drain |
| SIGHUP | Hot reload configuration |

## License

Apache-2.0
