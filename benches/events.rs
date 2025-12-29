//! Benchmarks for event bus operations.
//!
//! Run with: cargo bench --bench events

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use smppd::bootstrap::{Event, EventBus, EventCategory};
use smppd::store::MessageId;
use std::sync::Arc;
use tokio::runtime::Runtime;

fn create_message_received_event(id: u64) -> Event {
    Event::MessageReceived {
        id: MessageId::from_u64(id),
        system_id: "client_001".to_string(),
        destination: format!("+2588{:08}", id),
    }
}

fn create_message_delivered_event(id: u64) -> Event {
    Event::MessageDelivered {
        id: MessageId::from_u64(id),
        smsc_message_id: format!("SMSC{}", id),
        latency_us: 50000,
    }
}

fn bench_event_publish_no_subscribers(c: &mut Criterion) {
    let bus = EventBus::new(4096);

    c.bench_function("events/publish_no_subscribers", |b| {
        let mut id = 0u64;
        b.iter(|| {
            id += 1;
            let event = create_message_received_event(id);
            black_box(bus.publish(event))
        })
    });
}

fn bench_event_publish_with_subscribers(c: &mut Criterion) {
    let mut group = c.benchmark_group("events/publish_with_subscribers");

    for num_subs in [1, 10, 100].iter() {
        let bus = EventBus::new(4096);
        let _subscribers: Vec<_> = (0..*num_subs).map(|_| bus.subscribe()).collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(num_subs),
            num_subs,
            |b, _| {
                let mut id = 0u64;
                b.iter(|| {
                    id += 1;
                    let event = create_message_received_event(id);
                    black_box(bus.publish(event))
                })
            },
        );
    }

    group.finish();
}

fn bench_event_subscribe_receive(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("events/subscribe_and_receive", |b| {
        let bus = EventBus::new(4096);
        let mut rx = bus.subscribe();
        let mut id = 0u64;

        b.iter(|| {
            id += 1;
            let event = create_message_received_event(id);
            bus.publish(event);

            rt.block_on(async { black_box(rx.recv().await.unwrap()) })
        })
    });
}

fn bench_event_filtered_subscribe(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("events/filtered_subscription", |b| {
        let bus = EventBus::new(4096);
        let mut filtered_rx = bus.subscribe_filtered(vec![EventCategory::Message]);

        b.iter(|| {
            // Publish lifecycle event (should be filtered)
            bus.publish(Event::Ready);

            // Publish message event (should pass through)
            let event = create_message_received_event(1);
            bus.publish(event);

            rt.block_on(async { black_box(filtered_rx.recv().await.unwrap()) })
        })
    });
}

fn bench_event_category_classification(c: &mut Criterion) {
    let mut group = c.benchmark_group("events/category");

    group.bench_function("message_received", |b| {
        let event = create_message_received_event(1);
        b.iter(|| black_box(event.category()))
    });

    group.bench_function("lifecycle_ready", |b| {
        let event = Event::Ready;
        b.iter(|| black_box(event.category()))
    });

    group.bench_function("cluster_endpoint_up", |b| {
        let event = Event::EndpointUp {
            cluster: "vodacom".to_string(),
            endpoint: "smsc1:2775".to_string(),
        };
        b.iter(|| black_box(event.category()))
    });

    group.finish();
}

fn bench_event_is_critical(c: &mut Criterion) {
    let mut group = c.benchmark_group("events/is_critical");

    group.bench_function("critical_event", |b| {
        let event = Event::ShutdownStarted;
        b.iter(|| black_box(event.is_critical()))
    });

    group.bench_function("non_critical_event", |b| {
        let event = Event::Ready;
        b.iter(|| black_box(event.is_critical()))
    });

    group.finish();
}

fn bench_event_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("events/throughput");

    for batch_size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let bus = EventBus::new(batch_size * 2);
                let mut rx = bus.subscribe();

                b.iter(|| {
                    // Publish batch
                    for id in 0..batch_size {
                        let event = create_message_received_event(id as u64);
                        bus.publish(event);
                    }

                    // Receive all
                    rt.block_on(async {
                        for _ in 0..batch_size {
                            black_box(rx.recv().await.unwrap());
                        }
                    })
                })
            },
        );
    }

    group.finish();
}

fn bench_event_concurrent_publish(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    c.bench_function("events/concurrent_publish", |b| {
        let bus = Arc::new(EventBus::new(4096));
        let _rx = bus.subscribe(); // Keep channel open

        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();

                for t in 0..4 {
                    let bus = Arc::clone(&bus);
                    handles.push(tokio::spawn(async move {
                        for i in 0..100 {
                            let id = (t * 100 + i) as u64;
                            let event = create_message_received_event(id);
                            bus.publish(event);
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

fn bench_event_mixed_types(c: &mut Criterion) {
    c.bench_function("events/publish_mixed_types", |b| {
        let bus = EventBus::new(4096);
        let _rx = bus.subscribe();
        let mut id = 0u64;

        b.iter(|| {
            id += 1;

            // Cycle through different event types
            let event = match id % 5 {
                0 => Event::Starting,
                1 => create_message_received_event(id),
                2 => create_message_delivered_event(id),
                3 => Event::EndpointUp {
                    cluster: "test".to_string(),
                    endpoint: "ep:2775".to_string(),
                },
                _ => Event::SessionBound {
                    listener: "main".to_string(),
                    system_id: "client".to_string(),
                    bind_type: "trx".to_string(),
                },
            };

            black_box(bus.publish(event))
        })
    });
}

fn bench_event_stats(c: &mut Criterion) {
    use std::sync::atomic::Ordering;

    c.bench_function("events/stats_read", |b| {
        let bus = EventBus::new(4096);
        let _rx = bus.subscribe();

        // Publish some events
        for i in 0..1000 {
            bus.publish(create_message_received_event(i));
        }

        b.iter(|| {
            let stats = bus.stats();
            black_box(stats.published.load(Ordering::Relaxed));
            black_box(stats.dropped.load(Ordering::Relaxed));
            black_box(stats.by_category[EventCategory::Message as usize].load(Ordering::Relaxed))
        })
    });
}

criterion_group!(
    benches,
    bench_event_publish_no_subscribers,
    bench_event_publish_with_subscribers,
    bench_event_subscribe_receive,
    bench_event_filtered_subscribe,
    bench_event_category_classification,
    bench_event_is_critical,
    bench_event_throughput,
    bench_event_concurrent_publish,
    bench_event_mixed_types,
    bench_event_stats,
);

criterion_main!(benches);
