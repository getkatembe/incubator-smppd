use anyhow::Result;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    runtime,
    trace::{RandomIdGenerator, Sampler, Tracer},
    Resource,
};
use opentelemetry::KeyValue;
use tracing::info;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

/// Tracing configuration
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Service name
    pub service_name: String,

    /// Log level
    pub log_level: String,

    /// JSON log format
    pub json_logs: bool,

    /// OTLP endpoint (if set, enables OTEL export)
    pub otlp_endpoint: Option<String>,

    /// Sample rate (0.0 - 1.0)
    pub sample_rate: f64,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            service_name: "smppd".to_string(),
            log_level: "info".to_string(),
            json_logs: false,
            otlp_endpoint: None,
            sample_rate: 1.0,
        }
    }
}

/// Initialize tracing with optional OTEL export
pub fn init_tracing(config: &TracingConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    // Base subscriber with formatting layer
    let subscriber = tracing_subscriber::registry().with(env_filter);

    // Add format layer (JSON or pretty)
    if config.json_logs {
        let fmt_layer = fmt::layer()
            .json()
            .with_span_events(FmtSpan::CLOSE)
            .with_current_span(true)
            .with_target(true);

        if let Some(ref endpoint) = config.otlp_endpoint {
            // With OTEL
            let tracer = init_otlp_tracer(config, endpoint)?;
            let otel_layer = OpenTelemetryLayer::new(tracer);
            subscriber.with(fmt_layer).with(otel_layer).init();
        } else {
            subscriber.with(fmt_layer).init();
        }
    } else {
        let fmt_layer = fmt::layer()
            .pretty()
            .with_span_events(FmtSpan::CLOSE)
            .with_target(true);

        if let Some(ref endpoint) = config.otlp_endpoint {
            // With OTEL
            let tracer = init_otlp_tracer(config, endpoint)?;
            let otel_layer = OpenTelemetryLayer::new(tracer);
            subscriber.with(fmt_layer).with(otel_layer).init();
        } else {
            subscriber.with(fmt_layer).init();
        }
    }

    info!(
        service = %config.service_name,
        log_level = %config.log_level,
        json_logs = config.json_logs,
        otlp = config.otlp_endpoint.is_some(),
        "tracing initialized"
    );

    Ok(())
}

/// Initialize OTLP tracer
fn init_otlp_tracer(config: &TracingConfig, endpoint: &str) -> Result<Tracer> {
    let resource = Resource::new([
        KeyValue::new("service.name", config.service_name.clone()),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ]);

    let sampler = if config.sample_rate >= 1.0 {
        Sampler::AlwaysOn
    } else if config.sample_rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sample_rate)
    };

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("smppd");

    Ok(tracer)
}

/// Shutdown tracing (flush pending spans)
pub fn shutdown_tracing() {
    opentelemetry::global::shutdown_tracer_provider();
    info!("tracing shutdown complete");
}
