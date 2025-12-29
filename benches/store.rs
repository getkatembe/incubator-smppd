//! Benchmarks for message store operations.
//!
//! Run with: cargo bench --bench store

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use smppd::store::{InMemoryStore, MessageQuery, MessageState, MessageStore, Priority, StoredMessage};
use std::time::Duration;

fn create_test_message(seq: u32) -> StoredMessage {
    StoredMessage::new(
        &format!("+2588{:08}", seq),
        &format!("+2588{:08}", seq + 1000000),
        format!("Test message {}", seq).into_bytes(),
        "benchmark",
        seq,
    )
}

fn bench_store_single(c: &mut Criterion) {
    let store = InMemoryStore::new();
    let msg = create_test_message(1);

    c.bench_function("store/single_message", |b| {
        b.iter(|| {
            let m = msg.clone();
            black_box(store.store(m))
        })
    });
}

fn bench_store_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("store/batch");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let store = InMemoryStore::new();
            b.iter(|| {
                for i in 0..size {
                    let msg = create_test_message(i);
                    black_box(store.store(msg));
                }
            });
        });
    }

    group.finish();
}

fn bench_get_by_id(c: &mut Criterion) {
    let store = InMemoryStore::new();

    // Pre-populate store
    let mut ids = Vec::with_capacity(10000);
    for i in 0..10000 {
        let msg = create_test_message(i);
        ids.push(store.store(msg));
    }

    c.bench_function("store/get_by_id", |b| {
        let mut idx = 0;
        b.iter(|| {
            let id = ids[idx % ids.len()];
            idx += 1;
            black_box(store.get(id))
        })
    });
}

fn bench_get_pending(c: &mut Criterion) {
    let mut group = c.benchmark_group("store/get_pending");

    for total in [1000, 10000, 100000].iter() {
        let store = InMemoryStore::new();

        // Pre-populate with messages
        for i in 0..*total {
            let msg = create_test_message(i);
            store.store(msg);
        }

        group.bench_with_input(BenchmarkId::from_parameter(total), total, |b, _| {
            b.iter(|| black_box(store.get_pending(100)))
        });
    }

    group.finish();
}

fn bench_query_by_state(c: &mut Criterion) {
    let store = InMemoryStore::new();

    // Pre-populate with mixed states
    for i in 0..10000 {
        let mut msg = create_test_message(i);
        if i % 4 == 0 {
            msg.state = MessageState::Delivered;
        } else if i % 4 == 1 {
            msg.state = MessageState::Failed;
        } else if i % 4 == 2 {
            msg.state = MessageState::InFlight;
        }
        store.store(msg);
    }

    let query = MessageQuery {
        state: Some(MessageState::Pending),
        ..Default::default()
    };

    c.bench_function("store/query_by_state", |b| {
        b.iter(|| black_box(store.query(&query)))
    });
}

fn bench_query_by_system_id(c: &mut Criterion) {
    let store = InMemoryStore::new();

    // Pre-populate with different system_ids
    for i in 0..10000 {
        let mut msg = create_test_message(i);
        msg.system_id = format!("client_{}", i % 10);
        store.store(msg);
    }

    let query = MessageQuery {
        system_id: Some("client_5".to_string()),
        ..Default::default()
    };

    c.bench_function("store/query_by_system_id", |b| {
        b.iter(|| black_box(store.query(&query)))
    });
}

fn bench_update_state(c: &mut Criterion) {
    let store = InMemoryStore::new();

    // Pre-populate
    let mut ids = Vec::with_capacity(10000);
    for i in 0..10000 {
        let msg = create_test_message(i);
        ids.push(store.store(msg));
    }

    c.bench_function("store/update_state", |b| {
        let mut idx = 0;
        b.iter(|| {
            let id = ids[idx % ids.len()];
            idx += 1;
            store.update(
                id,
                Box::new(|m| {
                    m.state = MessageState::InFlight;
                }),
            );
        })
    });
}

fn bench_expire_old(c: &mut Criterion) {
    let mut group = c.benchmark_group("store/expire");

    for total in [1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(total), total, |b, &total| {
            b.iter_batched(
                || {
                    let store = InMemoryStore::new();
                    for i in 0..total {
                        let msg = create_test_message(i).with_ttl(Duration::ZERO);
                        store.store(msg);
                    }
                    store
                },
                |store| black_box(store.expire_old(Duration::ZERO)),
                criterion::BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

fn bench_priority_ordering(c: &mut Criterion) {
    let store = InMemoryStore::new();

    // Pre-populate with mixed priorities
    for i in 0..10000 {
        let mut msg = create_test_message(i);
        msg.priority = match i % 4 {
            0 => Priority::Critical,
            1 => Priority::High,
            2 => Priority::Normal,
            _ => Priority::Low,
        };
        store.store(msg);
    }

    c.bench_function("store/get_pending_priority_sorted", |b| {
        b.iter(|| black_box(store.get_pending(100)))
    });
}

fn bench_concurrent_access(c: &mut Criterion) {
    use std::sync::Arc;
    use std::thread;

    let store = Arc::new(InMemoryStore::new());

    // Pre-populate
    for i in 0..10000 {
        let msg = create_test_message(i);
        store.store(msg);
    }

    c.bench_function("store/concurrent_read_write", |b| {
        b.iter(|| {
            let store_clone = Arc::clone(&store);
            let handles: Vec<_> = (0..4)
                .map(|t| {
                    let s = Arc::clone(&store_clone);
                    thread::spawn(move || {
                        for i in 0..100 {
                            if t % 2 == 0 {
                                let msg = create_test_message(10000 + t * 100 + i);
                                s.store(msg);
                            } else {
                                s.get_pending(10);
                            }
                        }
                    })
                })
                .collect();

            for h in handles {
                h.join().unwrap();
            }
        })
    });
}

criterion_group!(
    benches,
    bench_store_single,
    bench_store_batch,
    bench_get_by_id,
    bench_get_pending,
    bench_query_by_state,
    bench_query_by_system_id,
    bench_update_state,
    bench_expire_old,
    bench_priority_ordering,
    bench_concurrent_access,
);

criterion_main!(benches);
