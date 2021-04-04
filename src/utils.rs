//! Utilities and constants for the MD@Home Rust implementation

/// Constants that describe the MD@Home rust client
pub mod constants {
    pub const PROG_NAME: &'static str = "Scalpel MD@Home";
    pub const SPEC: u16 = 30;
    pub const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    pub const REPO_URL: &'static str = env!("CARGO_PKG_REPOSITORY");
}

use std::time;

/// Basic timer implementation that can be used for benchmarking
///
/// # Example
///
/// ```rust
/// let timer = Timer::start();
/// // do a long task here...
/// log::debug!("time taken: {}ms", timer.elapsed());
/// ```
pub struct Timer(time::Instant);
impl Timer {
    pub fn start() -> Self {
        Self(time::Instant::now())
    }

    /// Gets how long has elapsed since the start of the timer in milliseconds
    pub fn elapsed(&self) -> f32 {
        self.0.elapsed().as_micros() as f32 / 1000f32
    }
}
