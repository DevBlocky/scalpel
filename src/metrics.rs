use crate::config::PromConfig;
#[cfg(target_os = "linux")]
use prometheus::process_collector::ProcessCollector;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    Result as PromResult, TextEncoder,
};

/// Registers the `$coll` to the `$registry`, returning the collector
macro_rules! prom_car {
    ($coll:expr, $registry:expr) => {{
        let collector = ($coll);
        ($registry)
            .register(Box::new(collector.clone()))
            .map(|_| collector)
    }};
}
/// Converts the `$ms` value into a value in seconds
macro_rules! ms {
    ($ms:expr) => {
        ($ms as f64) / 1000.0
    };
}

/// Prometheus Metrics client for the scalpel client.
pub struct Metrics {
    r: Registry,

    pub cache_size: IntGauge,

    pub total_requests: IntCounterVec,
    pub bytes_down: IntCounter,
    pub bytes_up: IntCounter,

    pub cache_latency: HistogramVec,
    pub request_duration: HistogramVec,
}

/// Default prometheus buckets for cache operations
const CACHE_DEFAULT_BUCKETS: &[f64] = &[
    ms!(0.25),
    ms!(0.5),
    ms!(1),
    ms!(2.5),
    ms!(5),
    ms!(7.5),
    ms!(10),
    ms!(15),
    ms!(20),
    ms!(30),
    ms!(50),
    ms!(100),
];
/// Default prometheus buckets for response durations
const RESPONSE_DEFAULT_BUCKETS: &[f64] = &[
    // these should generally apply to HITs
    ms!(1),
    ms!(2.5),
    ms!(5),
    ms!(10),
    ms!(30),
    ms!(75),
    // these should generally apply to MISSes
    ms!(150),
    ms!(300),
    ms!(500),
    ms!(750),
    ms!(1000),
    ms!(2000),
    ms!(5000),
];

/// Returns buckets that represent a length of time. If the provided `buckets` is not `None`, then
/// it assume the values are in milliseconds and convert them. If it is `None`, then it will use
/// `fallback` without any conversions.
fn tb_with_fallback<T: AsRef<[f64]>>(buckets: Option<T>, fallback: &[f64]) -> Vec<f64> {
    buckets
        .map(|x| x.as_ref().iter().map(|&x| ms!(x)).collect())
        .unwrap_or_else(|| Vec::from(fallback))
}

impl Metrics {
    /// Creates the new prometheus regiestry and registers all needed collectors
    pub fn new(opts: PromConfig) -> PromResult<Self> {
        let r = Registry::new();

        /* GAUGE METRICS */
        let cache_size = prom_car!(
            IntGauge::new("cache_reported_size", "Total size of the cache in bytes")?,
            r
        )?;

        /* COUNTER METRICS */
        let total_requests = prom_car!(
            IntCounterVec::new(
                Opts::new("requests_total", "The total amount of requests"),
                &["status"]
            )?,
            r
        )?;
        let bytes_down = prom_car!(
            IntCounter::new("bytes_down_total", "The total number of downloaded bytes")?,
            r
        )?;
        let bytes_up = prom_car!(
            IntCounter::new(
                "bytes_up_total",
                "The total number of uploaded (served) bytes"
            )?,
            r
        )?;

        /* HISTOGRAM_METRICS */
        let cache_latency = prom_car!(
            HistogramVec::new(
                HistogramOpts::new(
                    "cache_latency_seconds",
                    "Histogram of the number of seconds for a cache operation (load/save)"
                )
                // exponential bucket that starts from 1/5th of a millisecond
                .buckets(tb_with_fallback(
                    opts.cache_latency_buckets,
                    CACHE_DEFAULT_BUCKETS
                )),
                &["operation"]
            )?,
            r
        )?;
        let request_duration = prom_car!(
            HistogramVec::new(
                HistogramOpts::new(
                    "request_duration_seconds",
                    "The total number of seconds a request took to complete",
                )
                .buckets(tb_with_fallback(
                    opts.request_duration_buckets,
                    RESPONSE_DEFAULT_BUCKETS
                )),
                &["cache"],
            )?,
            r
        )?;

        // register program metrics (if on linux)
        #[cfg(target_os = "linux")]
        r.register(Box::new(ProcessCollector::for_self()))?;

        Ok(Self {
            r,
            cache_size,
            total_requests,
            bytes_down,
            bytes_up,
            cache_latency,
            request_duration,
        })
    }

    /// Encodes the metrics into a string to pass onto a metric store
    pub fn encode_to_string(&self) -> PromResult<String> {
        let mut buf = vec![];
        let encoder = TextEncoder::new();
        encoder.encode(&self.r.gather(), &mut buf)?;

        // this should be sound, because we're using the text encoder
        Ok(unsafe { String::from_utf8_unchecked(buf) })
    }

    // helper functions
    pub fn record_req(&self, status: &'static str) {
        self.total_requests.with_label_values(&[status]).inc();
    }
    pub fn record_cache_latency(&self, operation: &'static str, v: f64) {
        self.cache_latency
            .with_label_values(&[operation])
            .observe(v);
    }
    pub fn record_request_duration(&self, cache: &'static str, v: f64) {
        self.request_duration.with_label_values(&[cache]).observe(v);
    }
}
