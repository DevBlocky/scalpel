//! Utilities and constants for the MD@Home Rust implementation

/// Constants that describe the MD@Home rust client
pub mod constants {
    pub const PROG_NAME: &str = "Scalpel MD@Home";
    pub const SPEC: u16 = 31;
    pub const VERSION: &str = env!("CARGO_PKG_VERSION");
    pub const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
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
    #[inline]
    pub fn elapsed(&self) -> f32 {
        self.0.elapsed().as_micros() as f32 / 1000f32
    }

    /// Gets how long has elapsed since the start of the timer in seconds
    #[inline]
    pub fn elapsed_secs(&self) -> f32 {
        self.elapsed() / 1000f32
    }
}
impl std::fmt::Display for Timer {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        if fmt.alternate() {
            // print seconds elapsed
            write!(fmt, "{:.03}s", self.elapsed_secs())
        } else {
            // print milliseconds elapsed
            write!(fmt, "{:.03}ms", self.elapsed())
        }
    }
}
