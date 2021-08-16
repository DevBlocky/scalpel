use arc_swap::ArcSwap;
use std::sync::{atomic, Arc};
use std::time;

mod backend;
mod cache;
mod config;
mod http;
mod metrics;
mod tokens;
mod utils;

use backend::Backend;
pub use utils::constants;

static KILL_FLAG: atomic::AtomicBool = atomic::AtomicBool::new(false);

/// Structure that holds thread-safe data that should be accessible throughout most of the
/// application. This is created by the Application below and passed throughout the Application as
/// an Arc
pub struct GlobalState {
    config: Arc<config::AppConfig>,
    cache: Box<dyn cache::ImageCache>,
    verifier: ArcSwap<tokens::TokenVerifier>,
    backend: Backend,
    request_counter: atomic::AtomicUsize,
    metrics: metrics::Metrics,
}

/// Structure dedciated to holding MD@Home Rust lifetime logic
struct Application {
    gs: Arc<GlobalState>,
}

/// Dynamically creates the cache implementation based on the configured cache engine
///
/// ## Panic
///
/// This function will 100% of the time panic if there is a problem with the configuration of the
/// cache engine, there is an error creating the cache engine itself, or if the provided name is
/// invaid.
async fn create_dyn_cache(config: &config::AppConfig) -> Box<dyn cache::ImageCache> {
    match config.cache_engine.as_str() {
        #[cfg(feature = "ce-filesystem")]
        "fs" => Box::new(
            cache::FileSystemCache::new(config.fs_opt.as_ref().expect("fs ce config not provided"))
                .await
                .expect("unable to initialize fs cache engine"),
        ),
        #[cfg(feature = "ce-rocksdb")]
        "rocksdb" => Box::new(
            cache::RocksCache::new(
                config
                    .rocks_opt
                    .as_ref()
                    .expect("rocksdb ce config not provided"),
            )
            .expect("unable to initialize RocksDB cache engine"),
        ),
        a => panic!("\"{}\" is not a valid cache engine", a),
    }
}

impl Application {
    /// Creates a new Application based on a config, as well as starting the backend HTTP and
    /// pinging the backend.
    async fn new(config: config::AppConfig) -> Self {
        // initialize the global state Arc
        let gs = {
            // place config into it's own Arc so it can be shared to other portions of the
            // application without the entire global state
            //
            // this is mainly for the `Backend` module, as the `GlobalState` refers to the backend
            // structure and it wouldn't be wise to cyclically refer back to `GlobalState` inside
            // of the backend module
            let config = Arc::new(config);
            let metrics = metrics::Metrics::new().expect("metrics intialize");

            // may panic, but it's fine because it's before ping
            log::debug!("initializing cache...");
            let cache = create_dyn_cache(&config).await;

            // initialize the backend
            let backend = Backend::new(Arc::clone(&config));

            // create Atomic Reference Counter global state, that is passed to almost every aspect
            // of the application
            Arc::new(GlobalState {
                config,
                cache,
                backend,
                verifier: ArcSwap::from_pointee(tokens::TokenVerifier::new()),
                request_counter: atomic::AtomicUsize::new(0),
                metrics,
            })
        };

        Self { gs }
    }

    /// Pings the backend server (reporting any errors that occur), then returns the ssl
    /// certificate and whether this ssl certificate is new
    async fn ping_backend(
        &self,
    ) -> Result<Option<backend::TlsPayload>, Box<dyn std::error::Error>> {
        // perform the ping on the backend server
        let (crt, token_key) = self.gs.backend.ping().await?;

        // update the token verifier with the new token_key
        if let Some(token_key) = &token_key {
            let mut verifier = tokens::TokenVerifier::new();
            verifier.push_key_b64(token_key)?;
            self.gs.verifier.store(Arc::new(verifier));
        }

        // return certificate for HTTP server
        Ok(crt)
    }

    /// Shrinks the cache database if the reported size is above the maximum size in the config.
    /// Will log if an error occurs (but not the specific error) and the time it took.
    async fn try_shrink_db(&self) {
        // constant multipliers for cache threshold and shrink-to sizes
        // SHRINK_MULT = multiplier to the maximum size after shrinking, if shrink was triggered
        // MAX_MULT = multiplier to the max db size before triggering a shrink
        const SHRINK_MULT: f64 = 0.9;
        const MAX_MULT: f64 = 0.95;

        let db_sz = self.gs.cache.report() as f64;
        let max_sz = self.gs.config.cache_size_mebibytes as f64 * 1024f64 * 1024f64;
        log::info!(
            "reported cache size: {:.2}MiB ({:.2}%)",
            db_sz / 1024f64 / 1024f64,
            db_sz / max_sz * 100.0
        );
        self.gs.metrics.cache_size.set(db_sz as i64);
        self.gs.metrics.cache_max_size.set(max_sz as i64);

        // shrink database if reported size is above the maximum size reported in the config
        if db_sz > (max_sz * MAX_MULT) {
            log::warn!("database is over maximum size, shrinking...");
            let timer = utils::Timer::start();
            match self.gs.cache.shrink((max_sz * SHRINK_MULT) as u64).await {
                Ok(new_sz) => log::warn!("db shrinked to size {}B", new_sz),
                Err(_) => log::error!("problem shrinking database! hopefully there's more logs"),
            }
            log::info!("shrinking db took {}ms", timer.elapsed());
        }
    }

    /// Function that handles all the actions of the main thread.
    ///
    /// This function handles:
    /// - Registering the CTRL+C handler
    /// - Creating and orchestrating the HTTP Server
    /// - Updating the backend server with client settings
    /// - Shrinking the cache when it's oversized
    /// - Calls function to instigate graceful shutdown when CTRL+C is pressed
    async fn run(&mut self) {
        // perform initial ping to backend to get HTTP certificate
        // if API is trustworthy, then second "expect" should never panic
        let mut crt = self
            .ping_backend()
            .await
            .expect("error pinging backend on initial ping")
            .expect("TLS certificate wasn't provided in initial ping");

        // spawn the HTTP server with the certificate
        // if there is a problem creating it, gracefully shutdown and panic
        let mut server = match http::HttpServerLifecycle::new(Arc::clone(&self.gs), &crt) {
            Ok(srv) => srv,
            Err(e) => {
                log::error!("there was a problem creating the http server: {}", e);
                log::error!("gracefully shutting down then panic due to error...");
                self.shutdown(None).await;
                panic!("error creating HTTP server");
            }
        };

        let mut interval = tokio::time::interval(time::Duration::from_secs(1));
        let mut last_ping = time::Instant::now();
        // set last_shrink to 10 minutes ago so it'll try to shrink the db immediately
        let mut last_shrink = time::Instant::now() - time::Duration::from_secs(600);

        // run until we should begin shutdown sequence
        while !KILL_FLAG.load(atomic::Ordering::SeqCst) {
            interval.tick().await;

            // re-ping server every minute
            if last_ping.elapsed().as_secs() >= 60 {
                last_ping = time::Instant::now();
                // restart actix server if there is a new certificate
                match self.ping_backend().await {
                    Ok(Some(new_crt)) => {
                        crt = new_crt;
                        server.respawn_with_new_cert(&crt).await.unwrap();
                    }
                    Err(e) => log::error!("error pinging backend: {}", e),
                    _ => {} // pass-over
                }
            }

            // attempt to shrink the database every 5 minutes
            if last_shrink.elapsed().as_secs() >= 300 {
                last_shrink = time::Instant::now();
                self.try_shrink_db().await;
            }
        }

        // we are no longer running, we should begin graceful shutdown
        self.shutdown(Some(server)).await;
    }

    #[inline]
    fn get_num_requests(&self) -> usize {
        self.gs.request_counter.load(atomic::Ordering::Relaxed)
    }

    /// Function for gracefully shutting down the actix server and application as a whole. This
    /// function will wait until there are no more requests coming in OR that the time has exceeded
    /// the configured maximum grace period.
    ///
    /// This does not, however, gracefully shut down the actix server (wait for all keep-alives to
    /// drop) as that would take much time on top of the grace period.
    async fn shutdown(&self, server: Option<http::HttpServerLifecycle>) {
        // ping the backend server for stop, so that we'll stop receiving requests sometime soon
        log::info!("sending stop signal to API");
        if let Err(e) = self.gs.backend.stop().await {
            log::error!("error fulfilling stop API request: {}", e);
        }

        // wait until there are no more requests coming in
        let start = time::Instant::now();
        let mut requests = self.get_num_requests();
        let grace = self.gs.config.max_grace_period;
        loop {
            // immediately stop graceful shutdown if configured
            if grace < 0 {
                break;
            }
            tokio::time::sleep(time::Duration::from_secs(5)).await;

            // break if we've had no requests in the interval
            let x = self.get_num_requests();
            if x == requests {
                break;
            }
            requests = x;

            let elapsed = start.elapsed().as_secs() as i32;
            log::info!("waited for shutdown for {} seconds", elapsed);
            // break if we've waited for more seconds than the max grace period
            if grace != 0 && elapsed >= grace {
                break;
            }
        }

        if let Some(srv) = server {
            log::info!("shutting down actix web server");
            srv.shutdown(true).await;
        }
    }
}

async fn init() {
    // initialize sodiumoxide for thread safety
    sodiumoxide::init().expect("unable to initialize sodiumoxide");

    // load the configuration and turn into Arc, panic if it can't be loaded
    let config = config::init().await.unwrap_or_else(|| {
        log::error!("unable to find a valid configuration file. panic incoming...");
        panic!("no valid config");
    });

    // panic if cache size is less then minimum 40GiB
    if config.cache_size_mebibytes < 40960 {
        log::error!(
            "specified cache size must be at least 40GiB (40960MiB)! current: {}MiB",
            config.cache_size_mebibytes
        );
        panic!("cache size does not meet minimum requirements");
    }

    let mut app = Application::new(config).await;
    app.run().await;
}

fn main() {
    use env_logger::Env;

    // init the logger with INFO level
    env_logger::Builder::from_env(Env::default().default_filter_or("INFO")).init();

    ctrlc::set_handler(|| {
        log::warn!("stop signal received, setting kill flag");
        KILL_FLAG.store(true, atomic::Ordering::SeqCst);
    })
    .expect("ctrlc::set_handler");

    let max_bt: usize = std::env::var("TOKIO_MAX_BLOCKING_THREADS")
        .unwrap_or_else(|_| "512".to_string())
        .parse()
        .expect("env parse error");
    log::debug!("set tokio_max-blocking-threads: {}", max_bt);

    // create acitx system with custom tokio runtime
    log::debug!("bootstrapping tokio/actix runtime");
    let rt = actix_web::rt::System::with_tokio_rt(|| {
        tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(max_bt)
            .enable_all()
            .build()
            .expect("build tokio runtime")
    });

    rt.block_on(init())
}
