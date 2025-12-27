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
│   └── metrics/                 # Observability
│       ├── prometheus.go        # Prometheus exporter
│       └── stats.go             # Internal stats
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

### ConfigSource Interface

```go
// ConfigSource provides configuration (static or dynamic)
type ConfigSource interface {
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
