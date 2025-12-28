# smppd Implementation Plan (Rust)

smppd is built in Rust using smpp-rs as the protocol library.

## Architecture (Envoy-Inspired)

```
┌─────────────────────────────────────────────────────────────────┐
│                         smppd (Rust)                            │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Listener   │  │  Listener   │  │  Listener   │             │
│  │  (SMPP)     │  │  (HTTP)     │  │  (gRPC)     │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                     │
│         └────────────────┼────────────────┘                     │
│                          ▼                                      │
│  ┌─────────────────────────────────────────────────────────────┤
│  │                   Filter Chain (tower)                       │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │  │  Auth   │→│ RateLimit│→│ Firewall│→│  Route  │           │
│  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘           │
│  └─────────────────────────────────────────────────────────────┤
│                          │                                      │
│                          ▼                                      │
│  ┌─────────────────────────────────────────────────────────────┤
│  │                   Router                                     │
│  │  route match → cluster selection → load balancing            │
│  └─────────────────────────────────────────────────────────────┤
│                          │                                      │
│         ┌────────────────┼────────────────┐                     │
│         ▼                ▼                ▼                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Cluster    │  │  Cluster    │  │  Cluster    │             │
│  │  (vodacom)  │  │  (mtn)      │  │  (mock)     │             │
│  └─────────────┘  └─────────────┘  └─────────────┘             │
└─────────────────────────────────────────────────────────────────┘
```

## Tech Stack

| Component | Crate | Purpose |
|-----------|-------|---------|
| Protocol | `smpp-rs` | SMPP 3.3/3.4/5.0 |
| Runtime | `tokio` | Async I/O |
| Middleware | `tower` | Filter chain |
| HTTP API | `axum` | Admin/REST API |
| gRPC API | `tonic` | gRPC management |
| Config | `config` | YAML/JSON config |
| Logging | `tracing` | Structured logging |
| Metrics | `prometheus` | Prometheus export |
| CLI | `clap` | Command line |

## Crate Structure

```
smppd/
├── Cargo.toml
├── src/
│   ├── main.rs                # CLI entrypoint
│   ├── lib.rs                 # Library exports
│   │
│   ├── bootstrap/             # Process initialization
│   │   ├── mod.rs
│   │   ├── server.rs          # Main server struct
│   │   └── shutdown.rs        # Graceful shutdown
│   │
│   ├── config/                # Configuration
│   │   ├── mod.rs
│   │   ├── types.rs           # Config structs
│   │   ├── loader.rs          # YAML/JSON loader
│   │   ├── source.rs          # config::Source trait
│   │   └── watcher.rs         # Hot reload
│   │
│   ├── listener/              # Listener management
│   │   ├── mod.rs
│   │   ├── manager.rs         # Listener lifecycle
│   │   ├── smpp.rs            # SMPP listener (uses smpp-rs)
│   │   ├── http.rs            # HTTP listener (axum)
│   │   └── grpc.rs            # gRPC listener (tonic)
│   │
│   ├── filter/                # Filter chain (tower layers)
│   │   ├── mod.rs
│   │   ├── chain.rs           # Filter chain builder
│   │   ├── auth.rs            # Authentication
│   │   ├── ratelimit.rs       # Rate limiting
│   │   ├── firewall.rs        # SMS firewall
│   │   ├── transform.rs       # Message transform
│   │   ├── otp.rs             # OTP detection
│   │   ├── consent.rs         # Opt-out handling
│   │   ├── dlt.rs             # DLT validation (India)
│   │   ├── senderid.rs        # Sender ID validation
│   │   └── lua.rs             # Lua scripting
│   │
│   ├── router/                # Routing
│   │   ├── mod.rs
│   │   ├── router.rs          # Route matching
│   │   ├── matcher.rs         # Prefix/regex/lua matchers
│   │   └── priority.rs        # Priority routing
│   │
│   ├── cluster/               # Cluster management
│   │   ├── mod.rs
│   │   ├── manager.rs         # Cluster lifecycle
│   │   ├── cluster.rs         # Cluster struct
│   │   ├── loadbalancer/      # Load balancing
│   │   │   ├── mod.rs
│   │   │   ├── roundrobin.rs
│   │   │   ├── leastconn.rs
│   │   │   └── weighted.rs
│   │   └── health/            # Health checking
│   │       ├── mod.rs
│   │       └── enquire.rs     # SMPP enquire_link
│   │
│   ├── upstream/              # Upstream connections
│   │   ├── mod.rs
│   │   ├── pool.rs            # Connection pool
│   │   ├── conn.rs            # Single connection (smpp-rs Client)
│   │   └── mock.rs            # Mock responses
│   │
│   ├── credit/                # Credit control
│   │   ├── mod.rs
│   │   ├── controller.rs
│   │   └── store.rs           # Redis/memory store
│   │
│   ├── pricing/               # Dynamic pricing
│   │   ├── mod.rs
│   │   ├── engine.rs
│   │   └── rules.rs
│   │
│   ├── hlr/                   # HLR/MNP lookup
│   │   ├── mod.rs
│   │   ├── lookup.rs
│   │   ├── cache.rs
│   │   └── providers/
│   │       └── tyntec.rs
│   │
│   ├── campaign/              # Campaign management
│   │   ├── mod.rs
│   │   ├── manager.rs
│   │   └── scheduler.rs
│   │
│   ├── tenant/                # Multi-tenancy
│   │   ├── mod.rs
│   │   ├── manager.rs
│   │   └── quota.rs
│   │
│   ├── cdr/                   # CDR export
│   │   ├── mod.rs
│   │   ├── collector.rs
│   │   └── exporters/
│   │       ├── file.rs
│   │       ├── s3.rs
│   │       └── kafka.rs
│   │
│   ├── webhook/               # Webhook delivery
│   │   ├── mod.rs
│   │   ├── dispatcher.rs
│   │   └── retry.rs
│   │
│   ├── archive/               # Message archival
│   │   ├── mod.rs
│   │   └── storage.rs
│   │
│   ├── queue/                 # Message queue integration
│   │   ├── mod.rs
│   │   ├── producer.rs
│   │   ├── consumer.rs
│   │   └── kafka.rs
│   │
│   ├── bridge/                # Protocol bridge
│   │   ├── mod.rs
│   │   ├── http.rs            # HTTP → SMPP
│   │   └── grpc.rs            # gRPC → SMPP
│   │
│   ├── dlr/                   # DLR handling
│   │   ├── mod.rs
│   │   ├── harmonizer.rs      # Error code normalization
│   │   └── tracker.rs         # DLR correlation
│   │
│   ├── alerting/              # Alerting rules
│   │   ├── mod.rs
│   │   ├── rules.rs
│   │   └── notifier.rs
│   │
│   ├── extension/             # Extensions
│   │   ├── mod.rs
│   │   ├── recorder.rs        # Traffic recording
│   │   └── replayer.rs        # Traffic replay
│   │
│   ├── lua/                   # Lua scripting
│   │   ├── mod.rs
│   │   ├── vm.rs              # mlua VM pool
│   │   └── bindings.rs        # SMPP bindings
│   │
│   ├── admin/                 # Admin API
│   │   ├── mod.rs
│   │   ├── http.rs            # axum handlers
│   │   └── grpc.rs            # tonic service
│   │
│   ├── metrics/               # Observability
│   │   ├── mod.rs
│   │   └── prometheus.rs
│   │
│   └── mds/                   # Dynamic config (xDS-style)
│       ├── mod.rs
│       ├── client.rs
│       └── server.rs
│
├── proto/                     # Protobuf definitions
│   └── smppd/
│       └── v1/
│           ├── admin.proto
│           └── mds.proto
│
└── examples/
    ├── proxy.rs
    ├── router.rs
    └── mock_smsc.rs
```

## Dependencies

```toml
[package]
name = "smppd"
version = "0.1.0"
edition = "2024"

[dependencies]
# Protocol
smpp-rs = { path = "../smpp-rs" }

# Async runtime
tokio = { version = "1", features = ["full"] }

# Tower middleware
tower = { version = "0.5", features = ["full"] }
tower-service = "0.3"
tower-layer = "0.3"

# HTTP API
axum = { version = "0.8", features = ["ws"] }

# gRPC
tonic = "0.12"
prost = "0.13"

# Config
config = "0.14"
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"

# CLI
clap = { version = "4", features = ["derive"] }

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json"] }

# Metrics
prometheus = "0.13"

# Lua scripting
mlua = { version = "0.10", features = ["lua54", "async"] }

# Utils
bytes = "1"
thiserror = "2"
anyhow = "1"
async-trait = "0.1"
futures = "0.3"

[build-dependencies]
tonic-build = "0.12"

[features]
default = ["full"]
full = ["lua", "hlr", "campaign", "queue"]
lua = ["mlua"]
hlr = []
campaign = []
queue = ["rdkafka"]
```

## Implementation Phases

### Phase 1: Bootstrap & Config

**Goal:** smppd starts, loads YAML config

```
src/main.rs
src/bootstrap/mod.rs
src/bootstrap/server.rs
src/config/mod.rs
src/config/types.rs
src/config/loader.rs
```

**Deliverable:** `smppd -c config.yaml` starts

### Phase 2: SMPP Listener

**Goal:** Accept SMPP connections using smpp-rs

```
src/listener/mod.rs
src/listener/manager.rs
src/listener/smpp.rs
```

**Deliverable:** Clients can bind to smppd

```rust
// Uses smpp-rs Server
let server = smpp::Server::bind("0.0.0.0:2775")
    .serve(router)
    .await?;
```

### Phase 3: Upstream Connections

**Goal:** Connect to upstream SMSCs using smpp-rs Client

```
src/cluster/mod.rs
src/cluster/manager.rs
src/cluster/cluster.rs
src/upstream/mod.rs
src/upstream/pool.rs
src/upstream/conn.rs
src/cluster/health/enquire.rs
```

**Deliverable:** Messages forwarded upstream

```rust
// Uses smpp-rs Client
let client = smpp::Client::builder()
    .addr("smsc:2775")
    .auth("user", "pass")
    .build()?;
```

### Phase 4: Routing

**Goal:** Route messages based on destination

```
src/router/mod.rs
src/router/router.rs
src/router/matcher.rs
src/cluster/loadbalancer/mod.rs
src/cluster/loadbalancer/roundrobin.rs
```

**Deliverable:** Multi-carrier routing

### Phase 5: Filter Chain

**Goal:** Tower middleware for request processing

```
src/filter/mod.rs
src/filter/chain.rs
src/filter/auth.rs
src/filter/ratelimit.rs
```

**Deliverable:** Auth and rate limiting

```rust
let stack = ServiceBuilder::new()
    .layer(AuthLayer::new(clients))
    .layer(RateLimitLayer::new(100))
    .layer(RouterLayer::new(routes))
    .service(upstream_service);
```

### Phase 6: Mock Responses

**Goal:** Return configured responses without upstream

```
src/upstream/mock.rs
```

**Deliverable:** Test mode

### Phase 7: Admin API

**Goal:** HTTP API for management

```
src/admin/mod.rs
src/admin/http.rs
src/metrics/prometheus.rs
```

**Deliverable:** `/stats`, `/config`, `/health`

```rust
let app = Router::new()
    .route("/stats", get(stats_handler))
    .route("/config", get(config_handler))
    .route("/health", get(health_handler));
```

### Phase 8: Credit Control

```
src/credit/mod.rs
src/credit/controller.rs
src/credit/store.rs
src/filter/credit.rs
```

### Phase 9: SMS Firewall

```
src/filter/firewall.rs
```

### Phase 10: MDS (Dynamic Config)

```
src/mds/mod.rs
src/mds/client.rs
proto/smppd/v1/mds.proto
```

### Phase 11: Clustering

```
src/cluster/membership.rs
```

### Phase 12: Lua Scripting

```
src/lua/mod.rs
src/lua/vm.rs
src/lua/bindings.rs
src/filter/lua.rs
```

### Phase 13: DLR Harmonization

```
src/dlr/mod.rs
src/dlr/harmonizer.rs
src/dlr/codes.rs
```

### Phase 14: Protocol Bridge

```
src/bridge/mod.rs
src/bridge/http.rs
src/bridge/grpc.rs
```

### Phase 15: Message Queues

```
src/queue/mod.rs
src/queue/kafka.rs
```

### Phase 16: Alerting

```
src/alerting/mod.rs
src/alerting/rules.rs
```

### Phase 17: Record/Replay

```
src/extension/mod.rs
src/extension/recorder.rs
src/extension/replayer.rs
```

### Phase 18: HLR/MNP Lookup

```
src/hlr/mod.rs
src/hlr/lookup.rs
```

### Phase 19: OTP Handling

```
src/filter/otp.rs
```

### Phase 20: Dynamic Pricing

```
src/pricing/mod.rs
src/pricing/engine.rs
```

### Phase 21: Campaigns

```
src/campaign/mod.rs
src/campaign/manager.rs
```

### Phase 22: Multi-tenancy

```
src/tenant/mod.rs
```

### Phase 23: CDR Export

```
src/cdr/mod.rs
src/cdr/exporters/kafka.rs
```

### Phase 24: Webhooks

```
src/webhook/mod.rs
```

### Phase 25: Archival

```
src/archive/mod.rs
```

### Phase 26: Sender ID

```
src/filter/senderid.rs
```

### Phase 27: Consent/Opt-out

```
src/filter/consent.rs
```

### Phase 28: DLT (India)

```
src/filter/dlt.rs
```

## Key Integration: smpp-rs

smppd uses smpp-rs for all SMPP operations:

```rust
use smpp::{Server, Client, Router, Handler};
use smpp::pdu::{SubmitSm, DeliverSm};
use smpp::middleware::{Logger, RateLimit};

// Listener uses smpp-rs Server
async fn run_listener(config: ListenerConfig) -> Result<()> {
    let router = smpp::Router::new()
        .route(Command::SubmitSM, submit_handler)
        .layer(Logger::new());

    smpp::Server::bind(&config.addr)
        .serve(router)
        .await
}

// Upstream uses smpp-rs Client
async fn connect_upstream(config: UpstreamConfig) -> Result<smpp::Client> {
    smpp::Client::builder()
        .addr(&config.addr)
        .auth(&config.system_id, &config.password)
        .auto_reconnect(true)
        .build()
}

// Handler forwards to upstream
async fn submit_handler(
    session: smpp::Session,
    State(clusters): State<ClusterManager>,
    Request(pdu): Request<SubmitSm>,
) -> smpp::Response {
    // Route to cluster
    let cluster = clusters.route(&pdu).await?;

    // Forward to upstream
    let resp = cluster.send(pdu).await?;

    smpp::Response::ok(resp)
}
```

## Milestones

| Phase | Milestone | Status |
|-------|-----------|--------|
| 1 | smppd starts with config | ✅ Done |
| 2 | SMPP listener (smpp-rs) | ✅ Done |
| 3 | Upstream connections | ✅ Done |
| 4 | Multi-carrier routing | ✅ Done |
| 5 | Tower filter chain | ✅ Done |
| 6 | Mock responses | ✅ Done |
| 7 | Admin API (axum) | ✅ Done |
| 8 | Credit control | Skipped |
| 9 | SMS firewall | ✅ Done |
| 10 | MDS dynamic config | Pending |
| 11 | Clustering | Pending |
| 12 | Lua scripting | Pending |
| 13 | DLR harmonization | Pending |
| 14 | Protocol bridge | Pending |
| 15 | Message queues | Pending |
| 16 | Alerting rules | Pending |
| 17 | Record/Replay | Pending |
| 18 | HLR/MNP lookup | Pending |
| 19 | OTP handling | Pending |
| 20 | Dynamic pricing | Pending |
| 21 | Campaigns | Pending |
| 22 | Multi-tenancy | Pending |
| 23 | CDR export | Pending |
| 24 | Webhooks | Pending |
| 25 | Archival | Pending |
| 26 | Sender ID | Pending |
| 27 | Consent/Opt-out | Pending |
| 28 | DLT (India) | Pending |
