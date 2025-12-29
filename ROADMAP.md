# smppd Roadmap: World's Best SMPP Gateway

> **Vision**: Build the most performant, reliable, and feature-complete SMPP gateway that sets the industry standard.

## Excellence Criteria

| Criteria | Target | Current | Gap |
|----------|--------|---------|-----|
| **Throughput** | 1M+ msg/sec | ~50K (estimated) | Benchmarking needed |
| **Latency** | <1ms p99 | Unknown | Benchmarking needed |
| **Uptime** | 99.999% (5 nines) | Not measured | HA cluster needed |
| **Message Loss** | 0% | In-memory only | Persistent store needed |
| **Protocol** | SMPP 3.3/3.4/5.0 | 3.4 only | 3.3 + 5.0 needed |
| **Security** | SOC2/PCI compliant | Basic TLS | Audit logging, encryption |
| **Scalability** | Horizontal | Single node | Clustering needed |
| **Observability** | Full distributed tracing | Metrics only | Tracing enhancement |

---

## Phase 1: Foundation Excellence (Current Sprint)

### 1.1 Critical Fixes
- [x] Fix Cargo.toml edition (2024 → 2021)
- [ ] Implement session authentication (TODO in session.rs:301)
- [ ] Add per-cluster connection tracking (TODO in admin/server.rs:199)
- [ ] Pass system_id to firewall (TODO in firewall.rs:987)
- [ ] Fix filter chain cloning (TODO in chain.rs:116)

### 1.2 Testing Infrastructure
- [ ] End-to-end message flow tests
- [ ] Multi-cluster failover tests
- [ ] Hot reload under load tests
- [ ] Performance regression tests
- [ ] Chaos engineering tests (network partitions, latency injection)

### 1.3 Memory Safety
- [ ] Add memory bounds to message store (max messages, max bytes)
- [ ] Implement LRU eviction for old messages
- [ ] Add backpressure when store is full
- [ ] Memory profiling and leak detection

---

## Phase 2: Performance Excellence

### 2.1 Benchmarking Suite
```
benchmarks/
├── throughput/          # Messages per second
├── latency/            # P50, P95, P99, P999
├── connection/         # Connection establishment time
├── memory/             # Memory usage under load
└── cpu/               # CPU profiling
```

### 2.2 Optimizations
- [ ] Zero-copy PDU parsing
- [ ] Lock-free data structures for hot paths
- [ ] Connection pooling optimization
- [ ] Batched message processing
- [ ] SIMD for content filtering
- [ ] Custom allocator (jemalloc/mimalloc)

### 2.3 Async Improvements
- [ ] Replace RwLock with DashMap for concurrent access
- [ ] Implement work-stealing for load balancing
- [ ] Add priority queues for message scheduling
- [ ] Optimize Tokio runtime configuration

---

## Phase 3: Reliability Excellence

### 3.1 Persistent Storage
```rust
// Multiple storage backends
pub trait MessageStore: Send + Sync {
    async fn store(&self, msg: &Message) -> Result<MessageId>;
    async fn load(&self, id: MessageId) -> Result<Message>;
    async fn mark_delivered(&self, id: MessageId) -> Result<()>;
}

// Implementations
- InMemoryStore (current)
- RocksDbStore (durable, fast)
- PostgresStore (ACID, queryable)
- RedisStore (distributed cache)
```

### 3.2 High Availability
- [ ] Raft consensus for leader election
- [ ] Distributed message store
- [ ] Session state replication
- [ ] Active-passive failover
- [ ] Active-active clustering

### 3.3 Disaster Recovery
- [ ] Message journaling
- [ ] Point-in-time recovery
- [ ] Cross-region replication
- [ ] Backup/restore tooling

---

## Phase 4: Protocol Excellence

### 4.1 SMPP 3.3 Support
- [ ] Full PDU compatibility
- [ ] Legacy system integration
- [ ] Protocol auto-detection

### 4.2 SMPP 5.0 Support
- [ ] Extended TLV handling
- [ ] Unicode message support
- [ ] Enhanced delivery receipts
- [ ] Broadcast messaging

### 4.3 Protocol Extensions
- [ ] Custom TLV registry
- [ ] Vendor-specific extensions
- [ ] Protocol translation layer

---

## Phase 5: Security Excellence

### 5.1 Authentication & Authorization
```yaml
security:
  authentication:
    - type: password
      store: file|ldap|database
    - type: certificate
      ca: /path/to/ca.crt
    - type: oauth2
      provider: keycloak

  authorization:
    rbac:
      roles:
        - name: admin
          permissions: [read, write, admin]
        - name: sender
          permissions: [submit_sm]
```

### 5.2 Encryption
- [ ] TLS 1.3 with modern ciphers
- [ ] mTLS (mutual TLS)
- [ ] Message content encryption at rest
- [ ] Key rotation automation

### 5.3 Audit & Compliance
- [ ] Comprehensive audit logging
- [ ] PCI-DSS compliance features
- [ ] GDPR data handling
- [ ] SOC2 controls

---

## Phase 6: Observability Excellence

### 6.1 Distributed Tracing
```rust
// Every message gets a trace
#[instrument(skip(self), fields(
    message_id = %msg.id,
    source = %msg.source,
    destination = %msg.destination,
))]
async fn process_message(&self, msg: Message) -> Result<()> {
    // Spans for each processing stage
    let _filter = info_span!("filter").entered();
    self.filter_chain.apply(&msg).await?;

    let _route = info_span!("route").entered();
    let route = self.router.select(&msg).await?;

    let _forward = info_span!("forward").entered();
    self.forwarder.send(&msg, &route).await?;
}
```

### 6.2 Metrics Expansion
- [ ] Per-route latency histograms
- [ ] Per-customer throughput
- [ ] Error rate by error code
- [ ] Connection pool utilization
- [ ] Memory/CPU per component

### 6.3 Alerting Integration
- [ ] Prometheus AlertManager rules
- [ ] PagerDuty integration
- [ ] Slack notifications
- [ ] Custom webhook alerts

---

## Phase 7: Scalability Excellence

### 7.1 Horizontal Scaling
```
                    ┌─────────────┐
                    │   HAProxy   │
                    │ (L4/L7 LB)  │
                    └──────┬──────┘
           ┌───────────────┼───────────────┐
           ▼               ▼               ▼
    ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
    │   smppd-1   │ │   smppd-2   │ │   smppd-3   │
    └──────┬──────┘ └──────┬──────┘ └──────┬──────┘
           │               │               │
           └───────────────┼───────────────┘
                           ▼
                    ┌─────────────┐
                    │   Redis     │
                    │  (shared)   │
                    └─────────────┘
```

### 7.2 Kubernetes Native
```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: smppd
spec:
  replicas: 3
  template:
    spec:
      containers:
      - name: smppd
        resources:
          requests:
            memory: "1Gi"
            cpu: "500m"
          limits:
            memory: "4Gi"
            cpu: "2000m"
        livenessProbe:
          httpGet:
            path: /livez
            port: 9090
        readinessProbe:
          httpGet:
            path: /readyz
            port: 9090
```

### 7.3 Auto-scaling
- [ ] HPA based on message queue depth
- [ ] VPA for resource optimization
- [ ] Custom metrics adapter

---

## Phase 8: Developer Experience

### 8.1 SDKs
- [ ] Rust client SDK
- [ ] Go client SDK
- [ ] Python client SDK
- [ ] Node.js client SDK

### 8.2 APIs
- [ ] REST API (complete)
- [ ] gRPC API (high performance)
- [ ] WebSocket API (real-time)
- [ ] GraphQL API (flexible queries)

### 8.3 Documentation
- [ ] API reference (OpenAPI 3.0)
- [ ] Architecture guide
- [ ] Operations runbook
- [ ] Plugin development guide
- [ ] Performance tuning guide

---

## Phase 9: Enterprise Features

### 9.1 Multi-tenancy
```yaml
tenants:
  - name: customer_a
    quotas:
      messages_per_second: 10000
      max_connections: 100
    routes:
      - cluster: vodacom_a
    credentials:
      - system_id: CUST_A

  - name: customer_b
    quotas:
      messages_per_second: 5000
    routes:
      - cluster: mtn_shared
```

### 9.2 Billing & Metering
- [ ] Usage tracking per customer
- [ ] Rate plan configuration
- [ ] Invoice generation
- [ ] Prepaid/postpaid support

### 9.3 Management Console
- [ ] Web-based admin UI
- [ ] Real-time dashboards
- [ ] Configuration management
- [ ] User management

---

## Competitive Analysis

| Feature | smppd | Kannel | CloudHopper | Commercial |
|---------|-------|--------|-------------|------------|
| Performance | Target: Best | Medium | Good | Varies |
| Clustering | Planned | Limited | None | Yes |
| Plugin System | 4 types | C modules | Java | Limited |
| Modern Stack | Rust/Tokio | C | Java | Mixed |
| Cloud Native | Yes | No | Partial | Some |
| Open Source | Yes | Yes | Yes | No |

---

## Success Metrics

### Performance
- [ ] 1M messages/second sustained throughput
- [ ] <1ms P99 latency
- [ ] <100MB memory per 100K concurrent connections

### Reliability
- [ ] 99.999% uptime (5.26 minutes downtime/year)
- [ ] 0% message loss with persistence enabled
- [ ] <30 second failover time

### Adoption
- [ ] 1000+ GitHub stars
- [ ] 100+ production deployments
- [ ] Active community (Discord/Slack)
- [ ] Commercial support offering

---

## Timeline

| Phase | Duration | Status |
|-------|----------|--------|
| Phase 1: Foundation | 2 weeks | In Progress |
| Phase 2: Performance | 3 weeks | Planned |
| Phase 3: Reliability | 4 weeks | Planned |
| Phase 4: Protocol | 2 weeks | Planned |
| Phase 5: Security | 3 weeks | Planned |
| Phase 6: Observability | 2 weeks | Planned |
| Phase 7: Scalability | 4 weeks | Planned |
| Phase 8: DevEx | 3 weeks | Planned |
| Phase 9: Enterprise | 4 weeks | Planned |

**Total: ~27 weeks to world-class status**

---

## Next Actions

1. **Today**: Fix critical TODOs, add memory bounds
2. **This Week**: Benchmarking suite, integration tests
3. **Next Week**: RocksDB storage backend
4. **Month 1**: HA clustering, SMPP 5.0
5. **Month 2**: Security hardening, enterprise features
