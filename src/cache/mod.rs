use async_trait::async_trait;

// re-export different caches
// mod fs;
// pub use fs::FileSystemCache;
mod rocks;
pub use rocks::RocksCache;

/// A basic type representing an image in cache. First value represents the bytes and second value
/// represents an ETag (or unique identifier) for the image.
pub type ImageEntry = (Vec<u8>, String);

/// Trait for an MD@Home cache implementation.
///
/// Includes basic functions that would be used for
/// saving and loading images, plus extras that would be used for finding the size of the cache and
/// shrinking the cache size
///
/// All cache implementations that wish to mutate themselves during these calls must implement
/// interior mutability through thread-safe types in [`atomic`] or structures like [`RwLock`]
/// and [`Mutex`]. It is recommended to only lock these IF YOU HAVE TO (i.e. only during writes
/// to the DB, not reads) or else they could heavily affect performance on high workloads.
///
/// [`atomic`]: std::sync::atomic
/// [`RwLock`]: std::sync::RwLock
/// [`Mutex`]: std::sync::Mutex
#[async_trait]
pub trait ImageCache: Send + Sync {
    /// Load a cached image, returning a vector of bytes that represent the image and a timestamp
    /// of the last time that image was modified.
    ///
    /// Implementation should return None if the image is not cached or if there was an issue
    /// loading the image, otherwise return the bytes that represent the image and the timestamp
    /// that the image was saved.
    ///
    /// Implementation should also focus on this being as efficient as possible, and to use async
    /// wherever possible, as this will be called frequently
    async fn load(&self, chap_hash: &str, image: &str, saver: bool) -> Option<ImageEntry>;

    /// Save an image to the cache, returning whether it was successful.
    ///
    /// Implementation should return `true` if it was successfully saved, otherwise `false`. It is
    /// recommended for cache implementation to log if there was a problem as errors are not pushed
    /// up the stack.
    ///
    /// Implementation should also focus on this being as efficient as possible, and to use async
    /// wherever possible, as this can be called frequently
    async fn save(&self, chap_hash: &str, image: &str, saver: bool, data: &[u8]) -> bool;

    /// Reports the total size of the cache database in bytes.
    ///
    /// Function is not implemented in async because it is discouraged to constantly use
    /// long await calls to find cache size. Instead, implementation should implement a method that
    /// stores the cache size internally and automatically updates on save or shrink.
    fn report(&self) -> u64;

    /// Shrink the cache database to a minimum size.
    ///
    /// `min` is the minimum size the cache should shrink to in bytes.
    ///
    /// Implementation should return `Ok` with a new total cache size if successful. If there was
    /// an error, should return `Err(())`
    ///
    /// This is called infrequently, so it doesn't need to be efficient
    async fn shrink(&self, min: u64) -> Result<u64, ()>;
}
