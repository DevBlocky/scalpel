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

/// Time since epoch in milliseconds
#[inline]
pub fn now_as_millis() -> u64 {
    time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .map(|x| x.as_millis() as u64)
        .unwrap_or(0)
}

/// Struct that contains a secret of the client.
///
/// The struct will simply store the secret and allow for serialization/deserialization
/// without divulging the secret in debug outputs.
#[derive(Clone)]
pub struct Secret<T>(pub T);

impl<T> std::fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Hidden").finish()
    }
}
impl<T> std::ops::Deref for Secret<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: serde::Serialize> serde::Serialize for Secret<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Secret<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Secret)
    }
}
