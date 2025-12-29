//! Benchmarks for router operations.
//!
//! Run with: cargo bench --bench router

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use smppd::router::{MatchContext, Matcher, MatcherKind};

fn bench_matcher_prefix(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/prefix");

    // Short prefix
    group.bench_function("short_prefix", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    // Long prefix
    group.bench_function("long_prefix", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+25884123".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    // Mismatch
    group.bench_function("prefix_mismatch", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+27".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    group.finish();
}

fn bench_matcher_regex(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/regex");

    // Simple regex
    group.bench_function("simple_regex", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Regex(r"^\+258".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    // Complex regex with alternation
    group.bench_function("alternation_regex", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Regex(r"^\+258(84|82|86|87)".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    // Character class regex
    group.bench_function("char_class_regex", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Regex(r"^\+258[0-9]{9}$".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });

    group.finish();
}

fn bench_matcher_any(c: &mut Criterion) {
    c.bench_function("matcher/any", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Any)
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", None, None)))
    });
}

fn bench_matcher_with_source(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/multi_field");

    group.bench_function("dest_and_source", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .source(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", Some("+258821234567"), None)))
    });

    group.bench_function("dest_source_sender", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .source(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .sender_id(MatcherKind::Exact("COMPANY".to_string()))
            .unwrap()
            .build();

        b.iter(|| black_box(matcher.matches("+258841234567", Some("+258821234567"), Some("COMPANY"))))
    });

    group.finish();
}

fn bench_matcher_context(c: &mut Criterion) {
    c.bench_function("matcher/full_context", |b| {
        let matcher = Matcher::builder()
            .destination(MatcherKind::Prefix("+258".to_string()))
            .unwrap()
            .system_id("client_001".to_string())
            .service_type("SMS".to_string())
            .build();

        let ctx = MatchContext {
            destination: "+258841234567",
            source: Some("+258821234567"),
            sender_id: Some("COMPANY"),
            system_id: Some("client_001"),
            service_type: Some("SMS"),
            esm_class: Some(0),
            data_coding: Some(0),
        };

        b.iter(|| black_box(matcher.matches_context(&ctx)))
    });
}

fn bench_multiple_matchers(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/chain");

    for count in [5, 10, 20, 50].iter() {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(count), count, |b, &count| {
            // Create matchers - first matches at end for worst case
            let mut matchers: Vec<Matcher> = (0..count - 1)
                .map(|i| {
                    Matcher::builder()
                        .destination(MatcherKind::Prefix(format!("+{:03}", i)))
                        .unwrap()
                        .build()
                })
                .collect();

            // Add the matching one at the end
            matchers.push(
                Matcher::builder()
                    .destination(MatcherKind::Prefix("+258".to_string()))
                    .unwrap()
                    .build(),
            );

            b.iter(|| {
                for matcher in &matchers {
                    if matcher.matches("+258841234567", None, None) {
                        return black_box(true);
                    }
                }
                black_box(false)
            })
        });
    }

    group.finish();
}

fn bench_matcher_compilation(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/compilation");

    group.bench_function("prefix", |b| {
        b.iter(|| {
            black_box(
                Matcher::builder()
                    .destination(MatcherKind::Prefix("+258".to_string()))
                    .unwrap()
                    .build(),
            )
        })
    });

    group.bench_function("regex", |b| {
        b.iter(|| {
            black_box(
                Matcher::builder()
                    .destination(MatcherKind::Regex(r"^\+258(84|82|86|87)".to_string()))
                    .unwrap()
                    .build(),
            )
        })
    });

    group.finish();
}

fn bench_context_creation(c: &mut Criterion) {
    c.bench_function("context/creation", |b| {
        b.iter(|| {
            black_box(MatchContext {
                destination: "+258841234567",
                source: Some("+258821234567"),
                sender_id: Some("COMPANY"),
                system_id: Some("client_001"),
                service_type: Some("SMS"),
                esm_class: Some(0),
                data_coding: Some(0),
            })
        })
    });
}

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("matcher/throughput");

    let matcher = Matcher::builder()
        .destination(MatcherKind::Prefix("+258".to_string()))
        .unwrap()
        .build();

    for batch_size in [100, 1000, 10000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let destinations: Vec<String> = (0..batch_size)
                    .map(|i| format!("+2588{:08}", i))
                    .collect();

                b.iter(|| {
                    for dest in &destinations {
                        black_box(matcher.matches(dest, None, None));
                    }
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_matcher_prefix,
    bench_matcher_regex,
    bench_matcher_any,
    bench_matcher_with_source,
    bench_matcher_context,
    bench_multiple_matchers,
    bench_matcher_compilation,
    bench_context_creation,
    bench_throughput,
);

criterion_main!(benches);
