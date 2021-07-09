use super::{ImageCache, ImageEntry, ImageKey};
use crate::config::FsConfig;
use crate::utils::now_as_millis;
use bytes::Bytes;
use std::convert::TryInto;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub enum CacheError {
    Forceps(forceps::Error),
    Bincode(bincode::Error),
}

pub struct FileSystemCache {
    cache: forceps::Cache,

    /// timestamp of last full size fetch (millis since epoch)
    last_fetch: AtomicU64,
    /// total db bytes counter
    total: AtomicU64,
}

impl FileSystemCache {
    pub async fn new(config: &FsConfig) -> Result<Self, CacheError> {
        let cache = forceps::Cache::new(&config.path)
            .memory_lru_max_size(config.lru_size_mebibytes * 1024 * 1024)
            .read_write_buffer(config.rw_buffer_size * 1024)
            .build()
            .await
            .map_err(CacheError::Forceps)?;

        let s = Self {
            cache,
            last_fetch: AtomicU64::new(now_as_millis()),
            total: AtomicU64::new(0),
        };
        s.update_real_size();
        Ok(s)
    }

    /// Updates the internally kept database total bytes counter to the actual database value. This
    /// function is costly as it does an entire iteration over the database metadata.
    fn update_real_size(&self) -> u64 {
        let sz = self.cache.metadata_iter().fold(0u64, |acc, x| match x {
            Ok((_, meta)) => acc + meta.get_size(),
            _ => acc,
        });
        self.total.store(sz, Ordering::SeqCst);
        sz
    }

    /// Finds an estimated total size of the database. If a certain amount of time has passed, then
    /// it will find the real size and update the internal counter.
    fn find_size(&self) -> u64 {
        // 1 hr in milliseconds
        const MIN_TIME: u64 = 1000 * 60 * 60;
        let last_fetch = self.last_fetch.load(Ordering::Relaxed);
        let now = now_as_millis();

        // if it's been over an hour since the last fetch, then fetch again and update `last_fetch`
        // to the correct value
        if now - last_fetch >= MIN_TIME {
            self.update_real_size();
            self.last_fetch.store(now, Ordering::SeqCst);
        }

        self.total.load(Ordering::SeqCst)
    }

    /// Reads an entry from the database based on the key provided
    async fn read_from_db(&self, key: &ImageKey) -> Result<ImageEntry, CacheError> {
        let bytes = self
            .cache
            .read(key.as_bkey())
            .await
            .map_err(CacheError::Forceps)?;
        let e: ImageEntry = bytes.try_into().map_err(CacheError::Bincode)?;
        Ok(e)
    }

    /// Writes an entry to the database and returns the error that occurs
    async fn save_to_db(
        &self,
        key: &ImageKey,
        mime_type: String,
        data: Bytes,
    ) -> Result<(), CacheError> {
        let entry = ImageEntry::new_assume(data, mime_type);
        let ser_bytes: Bytes = entry.try_into().map_err(CacheError::Bincode)?;
        self.cache
            .write(key.as_bkey(), &ser_bytes)
            .await
            .map_err(CacheError::Forceps)?;

        // update the total size counter with the new storage value
        self.total
            .fetch_add(ser_bytes.len() as u64, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait::async_trait]
impl ImageCache for FileSystemCache {
    async fn load(&self, key: &ImageKey) -> Option<ImageEntry> {
        let res = self.read_from_db(key).await;
        match &res {
            Err(CacheError::Forceps(forceps::Error::NotFound)) => {}
            Err(e) => log::error!("error reading data from db: {}", e),
            _ => {}
        }
        res.ok()
    }
    async fn save(&self, key: &ImageKey, mime_type: String, data: Bytes) -> bool {
        if let Err(e) = self.save_to_db(key, mime_type, data).await {
            log::error!("error writing data to db: {}", e);
            false
        } else {
            true
        }
    }

    fn report(&self) -> u64 {
        self.find_size()
    }

    async fn shrink(&self, min: u64) -> Result<u64, ()> {
        use forceps::evictors::FifoEvictor;

        if let Err(e) = self.cache.evict_with(FifoEvictor::new(min)).await {
            log::error!("error shrinking db occured: {}", CacheError::Forceps(e));
            return Err(());
        }
        Ok(self.update_real_size())
    }
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Forceps(e) => write!(fmt, "ce-filesystem/forceps - \"{}\"", e),
            Self::Bincode(e) => write!(fmt, "ce-filesystem/bincode - \"{}\"", e),
        }
    }
}
impl std::error::Error for CacheError {}
