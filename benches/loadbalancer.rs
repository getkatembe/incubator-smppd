//! Benchmarks for load balancer operations.
//!
//! Run with: cargo bench --bench loadbalancer

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use smppd::cluster::{
    ConsistentHash, LeastConnections, LoadBalancer, LoadBalancerRegistry, PowerOfTwoChoices,
    Random, RoundRobin, Weighted, WeightedLeastConn,
};
use smppd::config::EndpointConfig;

fn create_endpoint_configs(count: usize) -> Vec<EndpointConfig> {
    (0..count)
        .map(|i| EndpointConfig {
            address: format!("smsc{}:2775", i),
            system_id: format!("sys{}", i),
            password: "password".to_string(),
            system_type: String::new(),
            weight: ((i % 3) + 1) as u32, // Weights: 1, 2, 3, 1, 2, 3...
            tls: None,
        })
        .collect()
}

fn bench_round_robin(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/round_robin");

    for count in [2, 5, 10, 50].iter() {
        let _configs = create_endpoint_configs(*count);
        let lb = RoundRobin::new();

        group.bench_with_input(BenchmarkId::from_parameter(count), count, |b, _| {
            b.iter(|| {
                // Simulate selection (we can't create real Endpoints easily)
                black_box(lb.name())
            })
        });
    }

    group.finish();
}

fn bench_weighted(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/weighted");

    for count in [2, 5, 10, 50].iter() {
        let configs = create_endpoint_configs(*count);
        let lb = Weighted::new(&configs);

        group.bench_with_input(BenchmarkId::from_parameter(count), count, |b, _| {
            b.iter(|| black_box(lb.name()))
        });
    }

    group.finish();
}

fn bench_least_connections(c: &mut Criterion) {
    c.bench_function("lb/least_conn/create", |b| {
        b.iter(|| black_box(LeastConnections::new()))
    });
}

fn bench_random(c: &mut Criterion) {
    c.bench_function("lb/random/select", |b| {
        let lb = Random::new();
        b.iter(|| black_box(lb.name()))
    });
}

fn bench_p2c(c: &mut Criterion) {
    c.bench_function("lb/p2c/select", |b| {
        let lb = PowerOfTwoChoices::new();
        b.iter(|| black_box(lb.name()))
    });
}

fn bench_consistent_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/consistent_hash");

    // Creation benchmark
    for count in [2, 5, 10, 50].iter() {
        let configs = create_endpoint_configs(*count);

        group.bench_with_input(
            BenchmarkId::new("create", count),
            count,
            |b, _| {
                b.iter(|| black_box(ConsistentHash::new(&configs, 100)))
            },
        );
    }

    // Selection by key
    let configs = create_endpoint_configs(10);
    let lb = ConsistentHash::new(&configs, 100);

    group.bench_function("select_by_key", |b| {
        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let key = format!("+2588{:08}", seq);
            black_box(lb.select_by_key(&key))
        })
    });

    group.finish();
}

fn bench_weighted_least_conn(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/weighted_least_conn");

    for count in [2, 5, 10, 50].iter() {
        let configs = create_endpoint_configs(*count);

        group.bench_with_input(BenchmarkId::from_parameter(count), count, |b, _| {
            b.iter(|| black_box(WeightedLeastConn::new(&configs)))
        });
    }

    group.finish();
}

fn bench_registry(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/registry");

    group.bench_function("create", |b| {
        b.iter(|| black_box(LoadBalancerRegistry::new()))
    });

    let registry = LoadBalancerRegistry::new();
    let configs = create_endpoint_configs(5);

    group.bench_function("lookup_round_robin", |b| {
        b.iter(|| black_box(registry.create("round_robin", &configs)))
    });

    group.bench_function("lookup_weighted", |b| {
        b.iter(|| black_box(registry.create("weighted", &configs)))
    });

    group.bench_function("lookup_consistent_hash", |b| {
        b.iter(|| black_box(registry.create("consistent_hash", &configs)))
    });

    group.bench_function("lookup_unknown", |b| {
        b.iter(|| black_box(registry.create("unknown_lb", &configs)))
    });

    group.finish();
}

fn bench_fnv_hash(c: &mut Criterion) {
    // Test the FNV hash implementation used by ConsistentHash
    c.bench_function("lb/fnv_hash", |b| {
        let configs = create_endpoint_configs(1);
        let lb = ConsistentHash::new(&configs, 1);

        let mut seq = 0u64;
        b.iter(|| {
            seq += 1;
            let key = format!("+258841234{:03}", seq % 1000);
            black_box(lb.select_by_key(&key))
        })
    });
}

fn bench_hash_distribution(c: &mut Criterion) {
    // Benchmark hash distribution quality
    c.bench_function("lb/hash_distribution_1000", |b| {
        let configs = create_endpoint_configs(10);
        let lb = ConsistentHash::new(&configs, 100);

        b.iter(|| {
            for i in 0..1000 {
                let key = format!("+2588{:08}", i);
                black_box(lb.select_by_key(&key));
            }
        })
    });
}

fn bench_lb_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/throughput");

    let configs = create_endpoint_configs(10);

    // Round-robin throughput
    for batch_size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("round_robin", batch_size),
            batch_size,
            |b, &batch_size| {
                let lb = RoundRobin::new();
                b.iter(|| {
                    for _ in 0..batch_size {
                        black_box(lb.name());
                    }
                })
            },
        );
    }

    // Consistent hash throughput
    for batch_size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("consistent_hash", batch_size),
            batch_size,
            |b, &batch_size| {
                let lb = ConsistentHash::new(&configs, 100);
                b.iter(|| {
                    for i in 0..batch_size {
                        let key = format!("+2588{:08}", i);
                        black_box(lb.select_by_key(&key));
                    }
                })
            },
        );
    }

    group.finish();
}

fn bench_lb_creation_cost(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/creation");

    let configs = create_endpoint_configs(10);

    group.bench_function("round_robin", |b| {
        b.iter(|| black_box(RoundRobin::new()))
    });

    group.bench_function("least_conn", |b| {
        b.iter(|| black_box(LeastConnections::new()))
    });

    group.bench_function("random", |b| {
        b.iter(|| black_box(Random::new()))
    });

    group.bench_function("p2c", |b| {
        b.iter(|| black_box(PowerOfTwoChoices::new()))
    });

    group.bench_function("weighted", |b| {
        b.iter(|| black_box(Weighted::new(&configs)))
    });

    group.bench_function("consistent_hash_100_replicas", |b| {
        b.iter(|| black_box(ConsistentHash::new(&configs, 100)))
    });

    group.bench_function("consistent_hash_1000_replicas", |b| {
        b.iter(|| black_box(ConsistentHash::new(&configs, 1000)))
    });

    group.bench_function("weighted_least_conn", |b| {
        b.iter(|| black_box(WeightedLeastConn::new(&configs)))
    });

    group.finish();
}

fn bench_memory_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("lb/memory");

    // Measure creation time as proxy for memory allocation
    for count in [10, 50, 100, 500].iter() {
        let configs = create_endpoint_configs(*count);

        group.bench_with_input(
            BenchmarkId::new("consistent_hash", count),
            count,
            |b, _| {
                b.iter(|| black_box(ConsistentHash::new(&configs, 100)))
            },
        );

        group.bench_with_input(
            BenchmarkId::new("weighted", count),
            count,
            |b, _| {
                b.iter(|| black_box(Weighted::new(&configs)))
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_round_robin,
    bench_weighted,
    bench_least_connections,
    bench_random,
    bench_p2c,
    bench_consistent_hash,
    bench_weighted_least_conn,
    bench_registry,
    bench_fnv_hash,
    bench_hash_distribution,
    bench_lb_throughput,
    bench_lb_creation_cost,
    bench_memory_overhead,
);

criterion_main!(benches);
