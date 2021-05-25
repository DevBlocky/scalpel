#[cfg(target_os = "linux")]
use prometheus::process_collector::ProcessCollector;
use prometheus::{
    histogram_opts, Encoder, Histogram, IntCounter, IntGauge, Registry, Result as PromResult,
    TextEncoder,
};

/// Macro that creates a struct with the `$struct_name` identifier that includes different prometheus metric definitions
///
/// It will automatically create a `new` method that will init all metrics with the specified registry. If there is an error,
/// it will be pushed up the stack using the Result type.
macro_rules! create_metrics {
    ($struct_name:ident, $(($name:ident: $ty:ty, $init:expr)),* $(,)?) => {
        pub struct $struct_name {
            $(
                pub $name: $ty,
            )*
        }
        impl $struct_name {
            fn new(r: &prometheus::Registry) -> prometheus::Result<Self> {
                $(
                    let $name = ($init);
                    r.register(Box::new($name.clone()))?;
                )*
                Ok(Self { $($name,)* })
            }
        }
    }
}
/// Basic array initializer that converts all values from milliseconds to seconds
macro_rules! ms {
    [$($v:expr),+ $(,)?] => {[
        $(($v) as f64 / 1000.0,)*
    ]}
}

/// Default prometheus buckets for cache operations
const CACHE_DEFAULT_BUCKETS: &[f64] = &ms![
    1.0 / 16.0,
    1.0 / 8.0,
    1.0 / 4.0,
    1.0 / 2.0,
    1.0,
    2.5,
    5.0,
    7.5,
    10.0,
    15.0,
    30.0,
    50.0,
    100.0,
];
/// Default prometheus buckets for response process durations
const PROCESS_DEFAULT_BUCKETS: &[f64] = &ms![
    // these should generally apply to HITs
    1.0 / 4.0,
    1.0 / 2.0,
    1.0,
    2.5,
    10.0,
    25.0,
    75.0,
    // these should generally apply to MISSes
    300.0,
    500.0,
    750.0,
    1000.0,
    2000.0,
    5000.0,
];

create_metrics!(
    MetricsInner,
    /* GAUGE METRICS */
    (
        cache_size: IntGauge,
        IntGauge::new("cache_reported_size", "Total size of the cache in bytes")?
    ),
    (
        cache_max_size: IntGauge,
        IntGauge::new(
            "cache_max_size",
            "Maximum size as specified in the configuration"
        )?
    ),
    /* COUNTER METRICS */
    (
        hit_requests_total: IntCounter,
        IntCounter::new("hit_requests_total", "Total HIT requests")?
    ),
    (
        miss_requests_total: IntCounter,
        IntCounter::new("miss_requests_total", "Total MISS requests")?
    ),
    (
        dropped_requests_total: IntCounter,
        IntCounter::new(
            "dropped_requests_total",
            "Total requests dropped because bad request"
        )?
    ),
    (
        failed_requests_total: IntCounter,
        IntCounter::new(
            "failed_requests_total",
            "Total requests that had an error while processing"
        )?
    ),
    (
        bytes_down: IntCounter,
        IntCounter::new("bytes_down_total", "The total number of downloaded bytes")?
    ),
    (
        bytes_up: IntCounter,
        IntCounter::new(
            "bytes_up_total",
            "The total number of uploaded (served) bytes"
        )?
    ),
    /* HISTOGRAM METRICS */
    (
        cache_load_seconds: Histogram,
        Histogram::with_opts(histogram_opts!(
            "cache_load_seconds",
            "Histogram that observes durations of cache loads",
            Vec::from(CACHE_DEFAULT_BUCKETS)
        ))?
    ),
    (
        cache_save_histo: Histogram,
        Histogram::with_opts(histogram_opts!(
            "cache_save_seconds",
            "Histogram that observces durations of cache saves",
            Vec::from(CACHE_DEFAULT_BUCKETS)
        ))?
    ),
    (
        hit_request_process_seconds: Histogram,
        Histogram::with_opts(histogram_opts!(
            "hit_request_process_seconds",
            "Observations for HIT request durations",
            Vec::from(PROCESS_DEFAULT_BUCKETS)
        ))?
    ),
    (
        miss_request_process_seconds: Histogram,
        Histogram::with_opts(histogram_opts!(
            "miss_request_process_seconds",
            "Observations for MISS request durations",
            Vec::from(PROCESS_DEFAULT_BUCKETS)
        ))?
    ),
    (
        upstream_ttfb_seconds: Histogram,
        Histogram::with_opts(histogram_opts!(
            "upstream_ttfb_seconds",
            "Upstream TTFB observations for MISS requests",
            Vec::from(PROCESS_DEFAULT_BUCKETS)
        ))?
    ),
);

/// Structure that contains all prometheus metrics of the scalpel program
///
/// All definitions are actually above and inside the `MetricsInner` class, but that can all be
/// accessed through this struct via `Deref`. It also includes a method of encoding to text for
/// scrapers.
pub struct Metrics {
    registry: Registry,
    inner: MetricsInner,
}

impl Metrics {
    /// Creates the new prometheus regiestry and registers all needed collectors
    pub fn new() -> PromResult<Self> {
        let registry = Registry::new();
        let inner = MetricsInner::new(&registry)?;

        // register program metrics (if on linux)
        #[cfg(target_os = "linux")]
        registry.register(Box::new(ProcessCollector::for_self()))?;

        Ok(Self { registry, inner })
    }

    /// Encodes the metrics into a string to pass onto a scraper
    pub fn encode_to_string(&self) -> PromResult<String> {
        let mut buf = vec![];
        let encoder = TextEncoder::new();
        encoder.encode(&self.registry.gather(), &mut buf)?;

        // this should be sound because we're using the text encoder
        Ok(unsafe { String::from_utf8_unchecked(buf) })
    }
}

impl std::ops::Deref for Metrics {
    type Target = MetricsInner;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
