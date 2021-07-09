use async_trait::async_trait;
use bytes::Bytes;
use std::convert::{TryFrom, TryInto};
use std::str::FromStr;
use std::sync::Arc;
use std::time;

// re-export different caches
#[cfg(feature = "ce-filesystem")]
mod fs;
#[cfg(feature = "ce-filesystem")]
pub use fs::FileSystemCache;

#[cfg(feature = "ce-rocksdb")]
mod rocks;
#[cfg(feature = "ce-rocksdb")]
pub use rocks::RocksCache;

#[derive(Debug)]
struct ImageKeyInner {
    chapter: String,
    image: String,
    data_saver: bool,
}

/// A data structure that represents the three components of an image path:
/// - The Chapter Hash
/// - The Image Name
/// - Whether it's `data` or `data-saver`
///
/// This data structure allows for shallow cloning that won't copy any actual memory
#[derive(Debug, Clone)]
pub struct ImageKey {
    inner: Arc<ImageKeyInner>,
}

impl ImageKey {
    /// Creates a new [`ImageKey`] instance using the provided `String`s
    pub fn new(chapter: String, image: String, data_saver: bool) -> Self {
        Self {
            inner: Arc::new(ImageKeyInner {
                chapter,
                image,
                data_saver,
            }),
        }
    }

    /// Retrieves the chapter hash associated with the key
    #[inline]
    pub fn chapter(&self) -> &str {
        &self.inner.chapter
    }
    /// Retrieves the file associated with the key
    #[inline]
    pub fn image(&self) -> &str {
        &self.inner.image
    }
    /// Retrieves if the key is data saver or not
    #[inline]
    pub fn data_saver(&self) -> bool {
        self.inner.data_saver
    }

    /// Returns a string representation of `data_saver`
    #[inline]
    pub fn archive_name(&self) -> &'static str {
        if self.data_saver() {
            "data-saver"
        } else {
            "data"
        }
    }

    /// Calculates a predicatable unqiue key for the chap_hash, image, saver combo
    ///
    /// Essentially calculates the md5 hash of the chapter hash and image name together, taking
    /// into account if the image is data-saver
    pub fn as_bkey(&self) -> Md5Bytes {
        let mut ctx = md5::Context::new();
        ctx.consume([self.data_saver() as u8]);
        ctx.consume(self.chapter());
        ctx.consume(self.image());
        ctx.compute().into()
    }
}

impl std::fmt::Display for ImageKey {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            fmt,
            "/{}/{}/{}",
            self.archive_name(),
            self.chapter(),
            self.image()
        )
    }
}

type Md5Bytes = [u8; 16];
/// A structure representing the data of an image in cache
///
/// This structure contains the data that makes up an image, with additional information included
/// for HTTP responses. It includes:
/// - Last Modified timestamp
/// - A checksum
/// - The mime type of the image
/// - The bytes of the image itself
#[derive(serde::Serialize, serde::Deserialize)]
pub struct ImageEntry {
    // milliseconds since epoch
    save_time: u128,
    checksum: Md5Bytes,
    mime_type: String,

    bytes_len: u64,
    bytes: Bytes,
}

impl ImageEntry {
    pub fn new(bytes: Bytes, mime_type: String, save_time: time::SystemTime) -> Self {
        Self {
            save_time: save_time
                .duration_since(time::UNIX_EPOCH)
                .map(|x| x.as_millis())
                .unwrap_or_default(),
            checksum: md5::compute(&bytes).into(),
            mime_type,
            bytes_len: bytes.len() as u64,
            bytes,
        }
    }

    /// Creates a new Image Entry based on the `bytes` and `mime_type` given
    ///
    /// This procedure will essentially "fill in the gaps," per se, for the `checksum` and
    /// `last_modified` parameters. Creating a new [`ImageEntry`] should only be done when saving a
    /// cache entry, not when loading. Instead, serde deserialization should be used for loading.
    #[inline]
    pub fn new_assume(bytes: Bytes, mime_type: String) -> Self {
        Self::new(bytes, mime_type, time::SystemTime::now())
    }

    /// Reference to the internal [`Bytes`] store
    #[inline]
    pub fn get_bytes(&self) -> Bytes {
        self.bytes.clone()
    }
    /// Total length of the bytes that make up the image
    #[inline]
    pub fn get_bytes_len(&self) -> u64 {
        self.bytes_len
    }
    /// Hexadecimal representation of the image checksum
    #[inline]
    pub fn get_checksum_hex(&self) -> String {
        hex::encode(&self.checksum)
    }

    /// The stored [`Mime`](mime::Mime) type of the image. Defaults to `image/png` if somehow
    /// corrupted or otherwise invalid.
    #[inline]
    pub fn get_mime(&self) -> mime::Mime {
        mime::Mime::from_str(&self.mime_type).unwrap_or(mime::IMAGE_PNG)
    }
}

impl TryInto<Bytes> for ImageEntry {
    type Error = bincode::Error;

    /// Serializes the datastructure into an array of bytes
    fn try_into(self) -> Result<Bytes, Self::Error> {
        // NOTE: converting from `Vec` to `Bytes` does not cause a Clone, so it is
        // memory/performance efficient
        bincode::serialize(&self).map(Bytes::from)
    }
}
impl TryFrom<Bytes> for ImageEntry {
    type Error = bincode::Error;

    /// Deserializes the datastructure from an array of bytes
    fn try_from(bytes: Bytes) -> Result<Self, Self::Error> {
        bincode::deserialize(&bytes)
    }
}

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
    /// Load a cached image, returning the [`ImageEntry`] structure that represents all of the data
    /// associated with that image.
    ///
    /// Implementation should return None if the image is not cached or if there was an issue
    /// loading the image, otherwise return the [`ImageEntry`] structure.
    ///
    /// Implementation should also focus on this being as efficient as possible, and to use async
    /// wherever possible, as this will be called frequently
    async fn load(&self, key: &ImageKey) -> Option<ImageEntry>;

    /// Save an image to the cache, returning whether it was successful.
    ///
    /// Implementation should return `true` if it was successfully saved, otherwise `false`. It is
    /// recommended for cache implementation to log if there was a problem as errors are not pushed
    /// up the stack.
    ///
    /// Implementations are also recommended to save images in the [`ImageEntry`] format, as it can
    /// be serialized and deserialized to bytes, and it is what the `load` function expects.
    ///
    /// Implementation should also focus on this being as efficient as possible, and to use async
    /// wherever possible, as this can be called frequently
    async fn save(&self, key: &ImageKey, mime_type: String, data: Bytes) -> bool;

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
