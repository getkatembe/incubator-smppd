//! Benchmarks for Write-Ahead Log operations.
//!
//! Run with: cargo bench --bench wal

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use smppd::store::{InMemoryWal, WalEntry, WriteAheadLog};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::runtime::Runtime;

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn create_store_entry(id: u64) -> WalEntry {
    WalEntry::Store {
        id,
        source: format!("+2588{:08}", id),
        destination: format!("+2588{:08}", id + 1000000),
        short_message: format!("Test message {}", id).into_bytes(),
        system_id: "benchmark".to_string(),
        sequence: id as u32,
        priority: 2, // Normal
        timestamp_ms: current_timestamp_ms(),
    }
}

fn create_update_entry(id: u64) -> WalEntry {
    WalEntry::UpdateState {
        id,
        state: 2, // InFlight
        cluster: Some("cluster1".to_string()),
        endpoint: Some("endpoint1:2775".to_string()),
        smsc_message_id: Some(format!("SMSC{}", id)),
        attempts: 1,
        timestamp_ms: current_timestamp_ms(),
    }
}

fn bench_wal_append_single(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("wal/inmemory/append_single", |b| {
        let wal = InMemoryWal::new();
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let entry = create_store_entry(id);
            rt.block_on(async { black_box(wal.append(entry).await.unwrap()) })
        })
    });
}

fn bench_wal_append_batch(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("wal/inmemory/append_batch");

    for size in [10, 100, 1000].iter() {
        group.throughput(Throughput::Elements(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let wal = InMemoryWal::new();
            let mut base_id = 0u64;

            b.iter(|| {
                let entries: Vec<_> = (0..size).map(|_| {
                    base_id += 1;
                    create_store_entry(base_id)
                }).collect();
                rt.block_on(async { black_box(wal.append_batch(entries).await.unwrap()) })
            })
        });
    }

    group.finish();
}

fn bench_wal_read_from(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("wal/inmemory/read_from");

    for total in [100, 1000, 10000].iter() {
        // Pre-populate WAL
        let wal = InMemoryWal::new();
        rt.block_on(async {
            for i in 0..*total {
                let entry = create_store_entry(i as u64);
                wal.append(entry).await.unwrap();
            }
        });

        group.bench_with_input(BenchmarkId::from_parameter(total), total, |b, _| {
            b.iter(|| {
                rt.block_on(async { black_box(wal.read_from(0).await.unwrap()) })
            })
        });
    }

    group.finish();
}

fn bench_wal_read_partial(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    // Pre-populate WAL with 10000 entries
    let wal = InMemoryWal::new();
    rt.block_on(async {
        for i in 0..10000 {
            let entry = create_store_entry(i as u64);
            wal.append(entry).await.unwrap();
        }
    });

    c.bench_function("wal/inmemory/read_from_middle", |b| {
        b.iter(|| {
            // Read from middle of WAL
            rt.block_on(async { black_box(wal.read_from(5000).await.unwrap()) })
        })
    });
}

fn bench_wal_truncate(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("wal/inmemory/truncate");

    for total in [1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(total), total, |b, &total| {
            b.iter_batched(
                || {
                    // Setup: create WAL with entries
                    let wal = InMemoryWal::new();
                    rt.block_on(async {
                        for i in 0..total {
                            let entry = create_store_entry(i as u64);
                            wal.append(entry).await.unwrap();
                        }
                    });
                    wal
                },
                |wal| {
                    // Truncate first half
                    rt.block_on(async {
                        black_box(wal.truncate_before((total / 2) as u64).await.unwrap())
                    })
                },
                criterion::BatchSize::SmallInput,
            )
        });
    }

    group.finish();
}

fn bench_wal_mixed_operations(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("wal/inmemory/mixed_store_update", |b| {
        let wal = InMemoryWal::new();
        let mut id = 0u64;

        b.iter(|| {
            id += 1;
            rt.block_on(async {
                // Store + Update pattern
                let store = create_store_entry(id);
                wal.append(store).await.unwrap();

                let update = create_update_entry(id);
                black_box(wal.append(update).await.unwrap())
            })
        })
    });
}

fn bench_wal_checkpoint(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("wal/inmemory/append_checkpoint", |b| {
        let wal = InMemoryWal::new();
        let mut seq = 0u64;

        b.iter(|| {
            seq += 1;
            let entry = WalEntry::Checkpoint {
                sequence: seq,
                timestamp_ms: current_timestamp_ms(),
            };
            rt.block_on(async { black_box(wal.append(entry).await.unwrap()) })
        })
    });
}

fn bench_wal_entry_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal/serialization");

    group.bench_function("store_entry", |b| {
        let entry = create_store_entry(1);
        b.iter(|| black_box(serde_json::to_vec(&entry).unwrap()))
    });

    group.bench_function("update_entry", |b| {
        let entry = create_update_entry(1);
        b.iter(|| black_box(serde_json::to_vec(&entry).unwrap()))
    });

    group.bench_function("store_entry_deserialize", |b| {
        let entry = create_store_entry(1);
        let bytes = serde_json::to_vec(&entry).unwrap();
        b.iter(|| black_box(serde_json::from_slice::<WalEntry>(&bytes).unwrap()))
    });

    group.finish();
}

fn bench_wal_concurrent_append(c: &mut Criterion) {
    use std::sync::Arc;
    let rt = Runtime::new().unwrap();

    c.bench_function("wal/inmemory/concurrent_append", |b| {
        let wal = Arc::new(InMemoryWal::new());

        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();
                for t in 0..4 {
                    let w = Arc::clone(&wal);
                    handles.push(tokio::spawn(async move {
                        for i in 0..25 {
                            let id = (t * 25 + i) as u64;
                            let entry = create_store_entry(id);
                            w.append(entry).await.unwrap();
                        }
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            })
        })
    });
}

criterion_group!(
    benches,
    bench_wal_append_single,
    bench_wal_append_batch,
    bench_wal_read_from,
    bench_wal_read_partial,
    bench_wal_truncate,
    bench_wal_mixed_operations,
    bench_wal_checkpoint,
    bench_wal_entry_serialization,
    bench_wal_concurrent_append,
);

criterion_main!(benches);
