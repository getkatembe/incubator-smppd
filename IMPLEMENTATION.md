# smppd Implementation Plan

## Architecture (Envoy-Inspired)

smppd follows the Envoy proxy architecture pattern:

```
┌─────────────────────────────────────────────────────────────────┐
│                         smppd process                           │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Listener   │  │  Listener   │  │  Listener   │             │
│  │  (SMPP)     │  │  (HTTP)     │  │  (gRPC)     │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                     │
│         └────────────────┼────────────────┘                     │
│                          ▼                                      │
│  ┌─────────────────────────────────────────────────────────────┤
│  │                   Filter Chain                               │
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
│         │                │                │                     │
│         ▼                ▼                ▼                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Endpoint   │  │  Endpoint   │  │  Mock       │             │
│  │  Pool       │  │  Pool       │  │  Response   │             │
│  └─────────────┘  └─────────────┘  └─────────────┘             │
└─────────────────────────────────────────────────────────────────┘
```

## Core Concepts (Envoy Mapping)

| Envoy Concept | smppd Equivalent | Description |
|---------------|------------------|-------------|
| Listener | Listener | Accepts connections (SMPP, HTTP, gRPC) |
| Filter Chain | Filter Chain | Request processing pipeline |
| Network Filter | Protocol Filter | SMPP codec, TLS termination |
| HTTP Filter | Message Filter | Auth, rate limit, firewall, transform |
| Router | Router | Route matching and cluster selection |
| Cluster | Cluster | Group of upstream endpoints |
| Endpoint | Upstream | Single SMSC connection |
| Health Check | Health Check | Upstream availability monitoring |
| xDS | MDS | Dynamic configuration discovery |

## Package Structure

```
smppd/
├── cmd/
│   └── smppd/
│       └── main.go              # CLI entrypoint
├── internal/
│   ├── bootstrap/               # Process initialization
│   │   ├── bootstrap.go         # Startup sequence
│   │   └── server.go            # Main server struct
│   ├── config/                  # Configuration
│   │   ├── config.go            # Root config struct
│   │   ├── listener.go          # Listener config
│   │   ├── cluster.go           # Cluster config
│   │   ├── route.go             # Route config
│   │   └── loader.go            # YAML/JSON loader
│   ├── listener/                # Listener management
│   │   ├── manager.go           # Listener lifecycle
│   │   ├── smpp.go              # SMPP listener (uses smpp-go)
│   │   ├── http.go              # HTTP listener
│   │   └── grpc.go              # gRPC listener
│   ├── filter/                  # Filter chain
│   │   ├── chain.go             # Filter chain execution
│   │   ├── filter.go            # Filter interface
│   │   ├── auth/                # Authentication filter
│   │   ├── ratelimit/           # Rate limiting filter
│   │   ├── firewall/            # SMS firewall filter
│   │   └── transform/           # Message transformation
│   ├── router/                  # Routing
│   │   ├── router.go            # Route matching
│   │   ├── matcher.go           # Route matchers (prefix, regex, lua)
│   │   └── config.go            # Route configuration
│   ├── cluster/                 # Cluster management
│   │   ├── manager.go           # Cluster lifecycle
│   │   ├── cluster.go           # Cluster struct
│   │   ├── loadbalancer/        # Load balancing
│   │   │   ├── lb.go            # LB interface
│   │   │   ├── roundrobin.go    # Round robin
│   │   │   ├── leastconn.go     # Least connections
│   │   │   └── weighted.go      # Weighted random
│   │   └── health/              # Health checking
│   │       ├── checker.go       # Health check runner
│   │       └── enquire.go       # SMPP enquire_link
│   ├── upstream/                # Upstream connections
│   │   ├── pool.go              # Connection pool
│   │   ├── conn.go              # Single connection
│   │   └── mock.go              # Mock responses
│   ├── credit/                  # Credit control
│   │   ├── controller.go        # Credit operations
│   │   └── store.go             # Balance storage
│   ├── admin/                   # Admin API
│   │   ├── server.go            # HTTP server
│   │   ├── handlers.go          # API handlers
│   │   └── grpc.go              # gRPC API
│   ├── metrics/                 # Observability
│   │   ├── prometheus.go        # Prometheus exporter
│   │   └── stats.go             # Internal stats
│   ├── alerting/                # Alerting rules
│   │   ├── manager.go           # Alert rule engine
│   │   ├── rules.go             # Rule definitions
│   │   └── notifier/            # Notification channels
│   │       ├── pagerduty.go     # PagerDuty integration
│   │       ├── slack.go         # Slack integration
│   │       └── webhook.go       # Generic webhook
│   ├── queue/                   # Message queue integration
│   │   ├── producer.go          # Queue producer interface
│   │   ├── consumer.go          # Queue consumer interface
│   │   ├── kafka/               # Kafka implementation
│   │   ├── rabbitmq/            # RabbitMQ implementation
│   │   ├── redis/               # Redis Streams implementation
│   │   └── nats/                # NATS implementation
│   ├── lua/                     # Lua scripting
│   │   ├── vm.go                # Lua VM pool
│   │   ├── sandbox.go           # Sandboxed execution
│   │   └── bindings.go          # SMPP bindings for Lua
│   ├── bridge/                  # Protocol bridge
│   │   ├── bridge.go            # Bridge interface
│   │   ├── http.go              # HTTP/REST bridge
│   │   ├── grpc.go              # gRPC bridge
│   │   └── kafka.go             # Kafka bridge
│   ├── dlr/                     # DLR handling
│   │   ├── harmonizer.go        # Error code harmonization
│   │   ├── codes.go             # Carrier code mappings
│   │   └── tracker.go           # DLR correlation
│   ├── extension/               # Extension system
│   │   ├── registry.go          # Extension registry
│   │   ├── recorder.go          # Traffic recorder
│   │   └── replayer.go          # Traffic replayer
│   ├── hlr/                     # Number lookup
│   │   ├── lookup.go            # HLR/MNP lookup interface
│   │   ├── cache.go             # Lookup result cache
│   │   └── providers/           # Lookup providers
│   │       ├── tyntec.go        # Tyntec HLR
│   │       └── hlrlookup.go     # HLRLookup.com
│   ├── otp/                     # OTP handling
│   │   ├── detector.go          # OTP pattern detection
│   │   └── priority.go          # Priority routing for OTP
│   ├── pricing/                 # Dynamic pricing
│   │   ├── engine.go            # Pricing engine
│   │   ├── rules.go             # Pricing rules
│   │   └── calculator.go        # Cost calculation
│   ├── compression/             # Message compression
│   │   ├── gsm7.go              # GSM-7 packing
│   │   └── multipart.go         # Multipart optimization
│   ├── campaign/                # Campaign management
│   │   ├── manager.go           # Campaign lifecycle
│   │   ├── scheduler.go         # Scheduled sending
│   │   └── throttle.go          # Rate control
│   ├── tenant/                  # Multi-tenancy
│   │   ├── manager.go           # Tenant management
│   │   ├── isolation.go         # Resource isolation
│   │   └── quota.go             # Tenant quotas
│   ├── cdr/                     # CDR export
│   │   ├── collector.go         # CDR collection
│   │   ├── exporter.go          # Export interface
│   │   └── exporters/           # Export destinations
│   │       ├── file.go          # File export
│   │       ├── s3.go            # S3 export
│   │       └── kafka.go         # Kafka export
│   ├── webhook/                 # Webhook delivery
│   │   ├── dispatcher.go        # Webhook dispatch
│   │   ├── retry.go             # Retry logic
│   │   └── signer.go            # Request signing
│   ├── archive/                 # Message archival
│   │   ├── archiver.go          # Archive interface
│   │   ├── storage.go           # Archive storage
│   │   └── search.go            # Archive search
│   ├── senderid/                # Sender ID management
│   │   ├── registry.go          # Sender ID registry
│   │   ├── validator.go         # Validation rules
│   │   └── rotation.go          # ID rotation
│   ├── consent/                 # Opt-out / Consent
│   │   ├── manager.go           # Consent management
│   │   ├── suppression.go       # Suppression list
│   │   └── stop.go              # STOP keyword handling
│   └── dlt/                     # DLT Registration (India)
│       ├── registry.go          # DLT template registry
│       ├── validator.go         # Template validation
│       └── scrubbing.go         # Header scrubbing
├── api/
│   └── smppd/
│       └── v1/
│           ├── smppd.proto      # gRPC definitions
│           └── mds.proto        # MDS discovery
└── pkg/
    └── xds/                     # MDS client/server
        ├── client.go            # MDS client
        └── server.go            # MDS server
```

## Implementation Phases

### Phase 1: Bootstrap & Static Config

**Goal:** smppd starts, loads config, accepts SMPP connections

```
cmd/smppd/main.go
internal/bootstrap/bootstrap.go
internal/bootstrap/server.go
internal/config/config.go
internal/config/loader.go
internal/listener/manager.go
internal/listener/smpp.go
```

**Deliverable:** `smppd -c config.yaml` starts and accepts SMPP binds

### Phase 2: Clusters & Upstreams

**Goal:** Connect to upstream SMSCs, forward messages

```
internal/config/cluster.go
internal/cluster/manager.go
internal/cluster/cluster.go
internal/upstream/pool.go
internal/upstream/conn.go
internal/cluster/health/checker.go
internal/cluster/health/enquire.go
```

**Deliverable:** Messages forwarded to upstream SMSCs

### Phase 3: Routing

**Goal:** Route messages based on destination, source, content

```
internal/config/route.go
internal/router/router.go
internal/router/matcher.go
internal/cluster/loadbalancer/lb.go
internal/cluster/loadbalancer/roundrobin.go
```

**Deliverable:** Multi-carrier routing with load balancing

### Phase 4: Filter Chain

**Goal:** Pluggable request processing pipeline

```
internal/filter/chain.go
internal/filter/filter.go
internal/filter/auth/auth.go
internal/filter/ratelimit/ratelimit.go
```

**Deliverable:** Auth and rate limiting working

### Phase 5: Mock Responses

**Goal:** Return configured responses without upstream

```
internal/upstream/mock.go
```

**Deliverable:** Test mode with mock SMSC responses

### Phase 6: Admin API

**Goal:** Runtime management and stats

```
internal/admin/server.go
internal/admin/handlers.go
internal/metrics/prometheus.go
```

**Deliverable:** `/stats`, `/config`, `/clusters` endpoints

### Phase 7: Credit Control

**Goal:** Track and enforce message credits

```
internal/credit/controller.go
internal/credit/store.go
internal/filter/credit/credit.go
```

**Deliverable:** Per-client credit limits

### Phase 8: SMS Firewall

**Goal:** Content filtering and fraud detection

```
internal/filter/firewall/firewall.go
internal/filter/firewall/rules.go
internal/filter/firewall/patterns.go
```

**Deliverable:** Spam/fraud blocking

### Phase 9: Dynamic Config (MDS)

**Goal:** xDS-style configuration discovery

```
api/smppd/v1/mds.proto
pkg/xds/client.go
pkg/xds/server.go
internal/config/dynamic.go
```

**Deliverable:** Hot config reload without restart

### Phase 10: Clustering

**Goal:** Multi-node deployment with state sync

```
internal/cluster/membership.go
internal/cluster/sync.go
```

**Deliverable:** HA deployment

### Phase 11: Lua Scripting

**Goal:** Custom routing and transformation logic via Lua

```
internal/lua/vm.go
internal/lua/sandbox.go
internal/lua/bindings.go
internal/router/lua.go
internal/filter/lua/lua.go
```

**Deliverable:** Lua scripts for routing decisions and message transforms

```lua
-- Example: route based on message content
function route(ctx, msg)
    if string.match(msg.short_message, "^OTP:") then
        return "priority-cluster"
    end
    return "default-cluster"
end
```

### Phase 12: DLR Harmonization

**Goal:** Normalize DLR error codes across carriers

```
internal/dlr/harmonizer.go
internal/dlr/codes.go
internal/dlr/tracker.go
internal/filter/dlr/dlr.go
```

**Deliverable:** Consistent DLR status regardless of carrier

```yaml
dlr_harmonization:
  carriers:
    vodacom:
      "001": { state: DELIVERED, error: 0 }
      "002": { state: UNDELIVERABLE, error: 1 }
    mtn:
      "DELIVRD": { state: DELIVERED, error: 0 }
      "EXPIRED": { state: EXPIRED, error: 2 }
```

### Phase 13: Protocol Bridge

**Goal:** Protocol translation (HTTP/gRPC/Kafka ↔ SMPP)

```
internal/bridge/bridge.go
internal/bridge/http.go
internal/bridge/grpc.go
internal/bridge/kafka.go
internal/listener/bridge.go
```

**Deliverable:** Send SMS via HTTP, receive MO/DLR via webhook

```
HTTP POST /v1/messages → SMPP submit_sm → Upstream
Upstream deliver_sm → Bridge → HTTP webhook callback
```

### Phase 14: Message Queues

**Goal:** Queue integration for async processing

```
internal/queue/producer.go
internal/queue/consumer.go
internal/queue/kafka/kafka.go
internal/queue/rabbitmq/rabbitmq.go
internal/queue/redis/redis.go
internal/queue/nats/nats.go
```

**Deliverable:** Submit via queue, receive DLR/MO via queue

```yaml
queues:
  submit:
    type: kafka
    brokers: [kafka:9092]
    topic: smpp.submit
  dlr:
    type: kafka
    brokers: [kafka:9092]
    topic: smpp.dlr
```

### Phase 15: Alerting Rules

**Goal:** Threshold-based alerts with notification channels

```
internal/alerting/manager.go
internal/alerting/rules.go
internal/alerting/notifier/pagerduty.go
internal/alerting/notifier/slack.go
internal/alerting/notifier/webhook.go
```

**Deliverable:** Alerts on error rates, latency, credit depletion

```yaml
alerting:
  rules:
    - name: high_error_rate
      condition: error_rate > 0.05
      for: 5m
      notify: [slack, pagerduty]
    - name: upstream_down
      condition: healthy_endpoints == 0
      notify: [pagerduty]
```

### Phase 16: Extensions (Record/Replay)

**Goal:** Traffic recording and replay for testing

```
internal/extension/registry.go
internal/extension/recorder.go
internal/extension/replayer.go
```

**Deliverable:** Capture production traffic, replay in test

```yaml
extensions:
  recorder:
    enabled: true
    output: /var/log/smppd/traffic.jsonl
    sample_rate: 0.1  # 10% of traffic
  replayer:
    enabled: false
    input: /var/log/smppd/traffic.jsonl
    speed: 2.0  # 2x speed
```

### Phase 17: Number Lookup (HLR/MNP)

**Goal:** Real-time number portability and validity lookup

```
internal/hlr/lookup.go
internal/hlr/cache.go
internal/hlr/providers/tyntec.go
internal/hlr/providers/hlrlookup.go
internal/filter/hlr/hlr.go
```

**Deliverable:** Route based on actual carrier, not prefix

```yaml
hlr:
  provider: tyntec
  api_key: ${TYNTEC_API_KEY}
  cache_ttl: 24h

routes:
  - match:
      hlr_network: "Vodacom"
    route:
      cluster: vodacom
```

### Phase 18: OTP Handling

**Goal:** Detect and prioritize OTP messages

```
internal/otp/detector.go
internal/otp/priority.go
internal/filter/otp/otp.go
```

**Deliverable:** OTP messages get priority routing

```yaml
otp:
  patterns:
    - "\\b\\d{4,8}\\b"
    - "OTP|PIN|code|verify"
  priority_cluster: fast-route
  metrics: true
```

### Phase 19: Dynamic Pricing

**Goal:** Time-based and volume-based pricing

```
internal/pricing/engine.go
internal/pricing/rules.go
internal/pricing/calculator.go
internal/filter/pricing/pricing.go
```

**Deliverable:** Cost varies by time, volume, destination

```yaml
pricing:
  default: 0.01
  rules:
    - match: { dest_prefix: "258" }
      price: 0.008
    - match: { time: "00:00-06:00" }
      discount: 0.2
    - match: { volume_tier: ">10000" }
      discount: 0.15
```

### Phase 20: Message Compression

**Goal:** Optimize message encoding

```
internal/compression/gsm7.go
internal/compression/multipart.go
internal/filter/compression/compression.go
```

**Deliverable:** Reduce multipart messages, pack GSM-7

```yaml
compression:
  gsm7_packing: true
  multipart_optimization: true
  unicode_to_gsm7: true  # Convert when possible
```

### Phase 21: Campaign Management

**Goal:** Bulk message campaigns with scheduling

```
internal/campaign/manager.go
internal/campaign/scheduler.go
internal/campaign/throttle.go
internal/admin/campaign.go
```

**Deliverable:** Upload CSV, schedule delivery, track progress

```yaml
campaigns:
  storage: postgres
  max_concurrent: 10
  default_throttle: 100/s
```

### Phase 22: Multi-tenancy

**Goal:** Isolated tenant environments

```
internal/tenant/manager.go
internal/tenant/isolation.go
internal/tenant/quota.go
internal/filter/tenant/tenant.go
```

**Deliverable:** Per-tenant config, quotas, billing

```yaml
tenants:
  - id: acme
    quotas:
      messages_per_day: 100000
      connections: 10
    routes: [acme-routes]
    upstreams: [acme-vodacom]
```

### Phase 23: CDR Export

**Goal:** Call detail records for billing/analytics

```
internal/cdr/collector.go
internal/cdr/exporter.go
internal/cdr/exporters/file.go
internal/cdr/exporters/s3.go
internal/cdr/exporters/kafka.go
```

**Deliverable:** Real-time CDR export

```yaml
cdr:
  format: json
  exporters:
    - type: kafka
      brokers: [kafka:9092]
      topic: cdr
    - type: s3
      bucket: cdr-archive
      prefix: daily/
```

### Phase 24: Webhooks

**Goal:** HTTP callbacks for events

```
internal/webhook/dispatcher.go
internal/webhook/retry.go
internal/webhook/signer.go
```

**Deliverable:** DLR/MO delivery via webhook

```yaml
webhooks:
  - url: https://api.example.com/sms/callback
    events: [dlr, mo]
    secret: ${WEBHOOK_SECRET}
    retry:
      max_attempts: 5
      backoff: exponential
```

### Phase 25: Message Archival

**Goal:** Long-term message storage for compliance

```
internal/archive/archiver.go
internal/archive/storage.go
internal/archive/search.go
```

**Deliverable:** Searchable message archive

```yaml
archive:
  enabled: true
  retention: 365d
  storage:
    type: s3
    bucket: sms-archive
  encryption: aes-256-gcm
```

### Phase 26: Sender ID Management

**Goal:** Sender ID validation and rotation

```
internal/senderid/registry.go
internal/senderid/validator.go
internal/senderid/rotation.go
internal/filter/senderid/senderid.go
```

**Deliverable:** Per-client sender ID rules

```yaml
sender_ids:
  - client: acme
    allowed: [ACME, AcmeCorp]
    default: ACME
  - client: "*"
    blocked: [BANK, GOVT]
```

### Phase 27: Opt-out / Consent

**Goal:** STOP keyword and suppression lists

```
internal/consent/manager.go
internal/consent/suppression.go
internal/consent/stop.go
internal/filter/consent/consent.go
```

**Deliverable:** Automatic opt-out handling

```yaml
consent:
  stop_keywords: [STOP, UNSUBSCRIBE, QUIT]
  suppression_storage: redis
  auto_reply: "You have been unsubscribed"
```

### Phase 28: DLT Registration (India)

**Goal:** TRAI DLT compliance for India

```
internal/dlt/registry.go
internal/dlt/validator.go
internal/dlt/scrubbing.go
internal/filter/dlt/dlt.go
```

**Deliverable:** Template validation, header scrubbing

```yaml
dlt:
  enabled: true
  entity_id: "1234567890"
  templates:
    - id: "TPL001"
      pattern: "Your OTP is {#var#}"
  scrub_headers: true
```

## Key Interfaces

### Filter Interface

```go
// Filter processes messages in the filter chain
type Filter interface {
    // Name returns the filter name for logging
    Name() string

    // OnBind is called when a client binds
    OnBind(ctx *Context, req *BindRequest) FilterStatus

    // OnSubmit is called for submit_sm
    OnSubmit(ctx *Context, req *SubmitRequest) FilterStatus

    // OnDeliver is called for deliver_sm (MO messages)
    OnDeliver(ctx *Context, req *DeliverRequest) FilterStatus

    // OnResponse is called for responses from upstream
    OnResponse(ctx *Context, resp *Response) FilterStatus
}

type FilterStatus int

const (
    Continue FilterStatus = iota  // Continue to next filter
    Stop                          // Stop chain, send response
    Async                         // Filter will call Continue() later
)
```

### LoadBalancer Interface

```go
// LoadBalancer selects an endpoint from a cluster
type LoadBalancer interface {
    // Pick selects the next endpoint
    Pick(ctx *Context, endpoints []*Endpoint) (*Endpoint, error)

    // Report feedback about a request (for adaptive LB)
    Report(endpoint *Endpoint, latency time.Duration, success bool)
}
```

### HealthChecker Interface

```go
// HealthChecker monitors endpoint health
type HealthChecker interface {
    // Start begins health checking
    Start(endpoint *Endpoint)

    // Stop ends health checking
    Stop(endpoint *Endpoint)

    // IsHealthy returns current health status
    IsHealthy(endpoint *Endpoint) bool
}
```

### config.Source Interface

```go
// Source provides configuration (static or dynamic)
// Located in internal/config/source.go
type Source interface {
    // Load returns the current configuration
    Load() (*Config, error)

    // Watch returns a channel that receives config updates
    Watch() <-chan *Config

    // Close stops watching
    Close() error
}
```

## Configuration Model

Following Envoy's model, configuration is hierarchical:

```yaml
# Bootstrap configuration (static)
node:
  id: smppd-1
  cluster: production

admin:
  address: 127.0.0.1:9901

# Static resources
static_resources:
  listeners:
    - name: smpp_listener
      address: 0.0.0.0:2775
      filter_chains:
        - filters:
            - name: smpp.auth
              config:
                clients_file: /etc/smppd/clients.yaml
            - name: smpp.ratelimit
              config:
                requests_per_second: 100
            - name: smpp.router
              config:
                routes: [route_table]

  clusters:
    - name: vodacom
      type: STRICT_DNS
      lb_policy: ROUND_ROBIN
      health_checks:
        - type: smpp.enquire_link
          interval: 30s
          timeout: 5s
      endpoints:
        - address: smsc1.vodacom.mz:2775
          weight: 100
        - address: smsc2.vodacom.mz:2775
          weight: 100

  routes:
    - name: route_table
      routes:
        - match:
            dest_addr_prefix: "258"
          route:
            cluster: vodacom

# Dynamic resources (MDS)
dynamic_resources:
  mds_config:
    api_type: GRPC
    grpc_services:
      - envoy_grpc:
          cluster_name: mds_cluster
```

## Envoy Patterns to Follow

### 1. Listener Drain

When removing/updating a listener:
1. Stop accepting new connections
2. Wait for in-flight requests to complete (drain timeout)
3. Close remaining connections
4. Remove listener

### 2. Cluster Warming

When adding a new cluster:
1. Create cluster in "warming" state
2. Establish connections to endpoints
3. Run health checks
4. Mark cluster "active" when healthy endpoints available

### 3. Request Context

Each request carries a context through the filter chain:
```go
type Context struct {
    // Unique request ID
    RequestID string

    // Client info
    Client *Client

    // Listener that received the request
    Listener *Listener

    // Selected route
    Route *Route

    // Selected cluster
    Cluster *Cluster

    // Selected endpoint
    Endpoint *Endpoint

    // Metadata (filter data)
    Metadata map[string]any

    // Timing
    StartTime time.Time

    // Logging
    Logger *zap.Logger
}
```

### 4. Stats Naming

Follow Envoy's hierarchical stats naming:
```
smppd.listener.smpp_listener.downstream_cx_total
smppd.listener.smpp_listener.downstream_cx_active
smppd.cluster.vodacom.upstream_cx_total
smppd.cluster.vodacom.upstream_rq_total
smppd.cluster.vodacom.upstream_rq_timeout
smppd.cluster.vodacom.health_check.success
smppd.cluster.vodacom.health_check.failure
smppd.cluster.vodacom.endpoint.10.0.0.1:2775.rq_success
```

### 5. Hot Restart

Support hot restart for zero-downtime upgrades:
1. New process starts, inherits listen sockets
2. New process signals ready
3. Old process starts draining
4. Old process exits after drain

## Dependencies

```go
require (
    github.com/katembe/smpp-go v0.0.0    // SMPP protocol
    github.com/spf13/cobra v1.8.1         // CLI
    github.com/spf13/viper v1.19.0        // Config
    go.uber.org/zap v1.27.0               // Logging
    github.com/prometheus/client_golang   // Metrics
    google.golang.org/grpc v1.69.2        // gRPC
    google.golang.org/protobuf v1.36.1    // Protobuf
    gopkg.in/yaml.v3 v3.0.1               // YAML
    github.com/yuin/gopher-lua            // Lua scripting
)
```

## Testing Strategy

### Unit Tests
- Each package has `*_test.go` files
- Mock interfaces for isolation
- Table-driven tests

### Integration Tests
- `internal/integration/` package
- Real SMPP connections (smpp-go client/server)
- Docker Compose for external dependencies

### E2E Tests
- `test/e2e/` directory
- Full smppd binary
- Real config files
- Testcontainers for SMSCs

## Milestones

| Phase | Milestone | Status |
|-------|-----------|--------|
| 1 | smppd accepts SMPP connections | Pending |
| 2 | Messages forwarded to upstreams | Pending |
| 3 | Multi-carrier routing works | Pending |
| 4 | Auth and rate limiting | Pending |
| 5 | Mock mode for testing | Pending |
| 6 | Admin API with stats | Pending |
| 7 | Credit control | Pending |
| 8 | SMS firewall | Pending |
| 9 | Dynamic config (MDS) | Pending |
| 10 | Clustering | Pending |
| 11 | Lua scripting | Pending |
| 12 | DLR harmonization | Pending |
| 13 | Protocol bridge | Pending |
| 14 | Message queues | Pending |
| 15 | Alerting rules | Pending |
| 16 | Record/Replay extensions | Pending |
| 17 | Number lookup (HLR/MNP) | Pending |
| 18 | OTP handling | Pending |
| 19 | Dynamic pricing | Pending |
| 20 | Message compression | Pending |
| 21 | Campaign management | Pending |
| 22 | Multi-tenancy | Pending |
| 23 | CDR export | Pending |
| 24 | Webhooks | Pending |
| 25 | Message archival | Pending |
| 26 | Sender ID management | Pending |
| 27 | Opt-out / Consent | Pending |
| 28 | DLT registration (India) | Pending |
