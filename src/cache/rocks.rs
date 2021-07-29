use super::{ImageCache, ImageEntry, ImageKey};
use crate::config::RocksConfig;
use crate::utils::now_as_millis;
use bytes::Bytes;
use rocksdb::{
    BoundColumnFamily, ColumnFamilyDescriptor, DBWithThreadMode, Error as DBError, IteratorMode,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

type MultiDB = DBWithThreadMode<rocksdb::MultiThreaded>;

#[derive(Debug)]
pub enum CacheError {
    Rocks(DBError),
    Bincode(bincode::Error),
    TokioJoin(tokio::task::JoinError),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        // TODO: do better here
        write!(fmt, "ce-rocksdb CacheError: {:?}", self)
    }
}

// functions that generate configuration options for RocksDb based on the client configuration

const MEBIBYTE: usize = 1024 * 1024;

fn block_cf_opts(conf: &RocksConfig) -> rocksdb::BlockBasedOptions {
    let mut opts = rocksdb::BlockBasedOptions::default();
    opts.set_format_version(5);

    // enable bloom filter if enabled in config
    if !conf.disable_bloom_filter {
        opts.set_bloom_filter(10, false);
        opts.set_cache_index_and_filter_blocks(true);
        opts.set_pin_l0_filter_and_index_blocks_in_cache(true);
    }

    // create lru config with specified size
    let lru_sz = conf.lru_size.unwrap_or(64);
    if lru_sz > 0 {
        if let Ok(lru) = rocksdb::Cache::new_lru_cache(lru_sz * MEBIBYTE) {
            opts.set_block_cache(&lru);
        }
    }

    opts
}
fn cf_opts(conf: &RocksConfig) -> rocksdb::Options {
    let mut cf_opts = rocksdb::Options::default();
    cf_opts.set_level_compaction_dynamic_level_bytes(true);
    cf_opts.set_block_based_table_factory(&block_cf_opts(conf));

    cf_opts
}
fn db_opts(conf: &RocksConfig) -> rocksdb::Options {
    let mut opts = rocksdb::Options::default();
    opts.create_missing_column_families(true);
    opts.create_if_missing(true);
    opts.set_compression_type(rocksdb::DBCompressionType::None);
    opts.set_keep_log_file_num(30);

    // number of background threads for the database
    opts.increase_parallelism(conf.parallelism.unwrap_or(2));

    // optimize compactions
    opts.set_max_background_jobs(6);
    opts.set_compaction_readahead_size(8 * MEBIBYTE);
    opts.optimize_level_style_compaction(512 * MEBIBYTE);

    // optimize writes
    if let Some(wrl) = conf.write_rate_limit {
        opts.set_ratelimiter((wrl * MEBIBYTE) as i64, 100_000, 10);
    }
    opts.set_write_buffer_size(conf.write_buffer_size.unwrap_or(64) * MEBIBYTE);

    // optimize reads
    opts.set_optimize_filters_for_hits(true);

    opts
}

#[derive(Debug)]
pub struct RocksCache {
    db: Arc<MultiDB>,

    db_size: AtomicU64,
    last_fetch: AtomicU64,
}

impl RocksCache {
    const IMAGES_CF: &'static str = "data";
    const META_CF: &'static str = "meta";

    pub fn new(conf: &RocksConfig) -> Result<Self, CacheError> {
        let image_cf = ColumnFamilyDescriptor::new(Self::IMAGES_CF, cf_opts(conf));
        let meta_cf = ColumnFamilyDescriptor::new(Self::META_CF, cf_opts(conf));

        let db = MultiDB::open_cf_descriptors(&db_opts(conf), &conf.path, vec![image_cf, meta_cf])
            .map_err(CacheError::Rocks)?;

        let this = Self {
            db: Arc::new(db),

            db_size: AtomicU64::new(0),
            last_fetch: AtomicU64::new(0),
        };
        this.fetch_real_size()?;
        Ok(this)
    }

    /// Obtains a ColumnFamily by name. Panics if the name provided does not exist.
    fn cf_by_name(&self, name: &'static str) -> Arc<BoundColumnFamily> {
        self.db.cf_handle(name).expect("cf handle name invalid")
    }

    /// Fetches the actual size of the database content by iterating through metadata.
    fn fetch_real_size(&self) -> Result<(), CacheError> {
        let mut sz = 0u64;

        let iter = self
            .db
            .iterator_cf(&self.cf_by_name(Self::META_CF), IteratorMode::Start);
        for (key, val) in iter {
            // attempt to deserialize the data and add the size to the `sz` iterator
            if let Ok(entry) = bincode::deserialize::<ImageEntry>(&val).map_err(CacheError::Bincode)
            {
                sz += entry.get_bytes_len();
                continue;
            }
            // drop all entries that could not be successfully deserialized
            self.drop_entry(&key)?;
        }

        // store the new size and the last fetch
        self.db_size.store(sz, Ordering::SeqCst);
        self.last_fetch.store(now_as_millis(), Ordering::SeqCst);
        Ok(())
    }

    /// Finds the saved size of the DB. It will occasionally fetch the real size.
    fn get_db_size(&self) -> Result<u64, CacheError> {
        // 1 hr in milliseconds
        const MIN_TIME: u64 = 1000 * 60 * 60;

        // if it's been over an hour since the last fetch, then re-fetch db size
        let now = now_as_millis();
        if now - self.last_fetch.load(Ordering::Relaxed) > MIN_TIME {
            self.fetch_real_size()?;
        }

        Ok(self.db_size.load(Ordering::SeqCst))
    }

    // Drops an entry from the data and metadata column families.
    fn drop_entry(&self, key: &[u8]) -> Result<(), CacheError> {
        self.db
            .delete_cf(&self.cf_by_name(Self::IMAGES_CF), key)
            .map_err(CacheError::Rocks)?;
        self.db
            .delete_cf(&self.cf_by_name(Self::META_CF), key)
            .map_err(CacheError::Rocks)?;
        Ok(())
    }

    /// Function that will spawn a blocking threat to perform an async db operation.
    async fn db_op_async<R, F>(&self, f: F) -> Result<R, CacheError>
    where
        R: Send + 'static,
        F: FnOnce(&MultiDB) -> Result<R, CacheError> + Send + 'static,
    {
        let db = Arc::clone(&self.db);

        // spawn the tokio task that will call the function to perform the async db operation
        tokio::task::spawn_blocking(move || f(&db))
            .await
            .map_err(CacheError::TokioJoin)
            .and_then(|x| x)
    }
    /// Utilizes `db_op_async` place an item in a Column Family with async
    async fn put_cf_async(
        &self,
        cf_name: &'static str,
        key: Bytes,
        val: Bytes,
    ) -> Result<(), CacheError> {
        self.db_op_async(move |db| {
            // find the ColumnFamily by name
            let cf = db.cf_handle(cf_name).expect("cf_handle non-existant");

            // place the entry into the database
            db.put_cf(&cf, &key, &val).map_err(CacheError::Rocks)
        })
        .await
    }
    /// Utilizes `db_op_async` to obtain the bytes of an entry in a Column Family with async
    async fn get_cf_async(
        &self,
        cf_name: &'static str,
        key: Bytes,
    ) -> Result<Option<Bytes>, CacheError> {
        self.db_op_async(move |db| {
            // find the ColumnFamily by name
            let cf = db.cf_handle(cf_name).expect("cf_handle non-existant");

            // fetch from the db and convert from Vec<u8> to Bytes
            db.get_cf(&cf, &key)
                .map(|x| x.map(|x| Bytes::from(x)))
                .map_err(CacheError::Rocks)
        })
        .await
    }

    /// Saves an ImageEntry to the database at the specified key
    ///
    /// Returns early if an error occurred on any DB operation
    async fn save_entry(&self, key: &ImageKey, mut entry: ImageEntry) -> Result<(), CacheError> {
        use std::convert::TryInto;
        let bkey = Bytes::copy_from_slice(&key.as_bkey());

        // create the future that will save the image data
        let bytes = std::mem::replace(&mut entry.bytes, Bytes::new());
        let images_fut = self.put_cf_async(Self::IMAGES_CF, bkey.clone(), bytes);

        // create the future that will save the metadata (first omitting the bytes)
        let len = entry.get_bytes_len();
        let meta_fut = self.put_cf_async(
            Self::META_CF,
            bkey,
            entry.try_into().map_err(CacheError::Bincode)?,
        );

        // update the db size counter
        self.db_size.fetch_add(len, Ordering::Relaxed);

        tokio::try_join!(images_fut, meta_fut)?;
        Ok(())
    }
    /// Loads an ImageEntry from the database at the specified key
    ///
    /// Returns early if an error occurred on any DB operation
    async fn load_entry(&self, key: &ImageKey) -> Result<Option<ImageEntry>, CacheError> {
        use std::convert::TryFrom;
        let bkey = Bytes::copy_from_slice(&key.as_bkey());

        // load the entire image entry from the database
        let images_fut = self.get_cf_async(Self::IMAGES_CF, bkey.clone());
        let meta_fut = self.get_cf_async(Self::META_CF, bkey);

        // wait for both futures and deserialize
        match tokio::try_join!(images_fut, meta_fut)? {
            // if there is data for both cfs, then integrate data and return
            (Some(data), Some(meta)) => {
                let mut entry = ImageEntry::try_from(meta).map_err(CacheError::Bincode)?;
                entry.bytes = data;
                Ok(Some(entry))
            }
            _ => Ok(None),
        }
    }

    /// Eviction algorithm to evict the oldest entries in the database
    fn evict_entries_fifo(&self, until_size: u64) -> Result<u64, CacheError> {
        // make sure we're working with the actual db size
        self.fetch_real_size()?;
        let mut sz = self.get_db_size()?;

        'evictor: loop {
            // create a queue of entries to evict based on the save time of the entry
            // this queue is automatically sorted based on the find_top_entries fn
            let queue = self.find_top_entries(256, |x, y| y.save_time.cmp(&x.save_time))?;

            // how did we get here? we'll break anyways but how
            if queue.len() == 0 {
                log::debug!("how did we get here?");
                break;
            }

            // drop entries in the queue until we meet the minimum size
            // drops are considered fatal, so it'll be pushed up the stack if failed
            // if minimum size isn't met, then 'evictor loop will continue around, building a new queue
            for (key, entry) in queue {
                self.drop_entry(&key)?;
                sz -= entry.get_bytes_len();

                if sz <= until_size {
                    log::debug!("{} <= {}", sz, until_size);
                    break 'evictor;
                }
            }
        }

        self.db_size.store(sz, Ordering::SeqCst);
        Ok(sz)
    }

    /// Returns a vector of `n` number of  ImageKey and ImageEntry pairs that best fit the
    /// comparator provided.
    ///
    /// WARNING: This function is not fast and it's not intended to be fast. Use with care.
    fn find_top_entries<C>(
        &self,
        n: usize,
        comparator: C,
    ) -> Result<Vec<(Box<[u8]>, ImageEntry)>, CacheError>
    where
        C: Fn(&ImageEntry, &ImageEntry) -> std::cmp::Ordering,
    {
        let mut acc = Vec::with_capacity(n);

        let iter = self
            .db
            .iterator_cf(&self.cf_by_name(Self::META_CF), IteratorMode::Start);
        for (key, val) in iter {
            // deserialize the metadata entry, if it fails then drop it from db
            let entry = match bincode::deserialize::<ImageEntry>(&val) {
                Ok(e) => e,
                Err(_) => {
                    self.drop_entry(&key)?;
                    continue;
                }
            };

            // if accumulator isn't filled yet, then just add the entry
            if acc.len() < n {
                acc.push((key, entry));
                continue;
            }

            // the accumulator is completely filled, so replace based on comparator
            //
            // in this case, we use the `find` operation to find an entry that should be evicted
            // with less priority than the `entry` variable. if found, then replace entry in accumulator
            if let Some((i, _)) = acc
                .iter()
                .enumerate()
                .find(|(_, (_, other))| comparator(&entry, other).is_gt())
            {
                acc[i] = (key, entry);
            }
        }

        // as a finishing touch, completely sort the accumulator
        acc.sort_unstable_by(|(_, first), (_, second)| comparator(first, second).reverse());
        Ok(acc)
    }
}

#[async_trait::async_trait]
impl ImageCache for RocksCache {
    async fn load(&self, key: &ImageKey) -> Option<ImageEntry> {
        match self.load_entry(key).await {
            Ok(entry) => entry,
            Err(e) => {
                log::error!("fatal error occurred loading entry from RocksDb: {}", e);
                None
            }
        }
    }

    async fn save(&self, key: &ImageKey, mime_type: String, data: Bytes) -> bool {
        let entry = ImageEntry::new_assume(data, mime_type);
        if let Err(e) = self.save_entry(key, entry).await {
            log::error!("fatal error occurred saving entry to RocksDb: {}", e);
            false
        } else {
            true
        }
    }

    fn report(&self) -> u64 {
        self.get_db_size().unwrap_or_default()
    }

    async fn shrink(&self, min: u64) -> Result<u64, ()> {
        self.evict_entries_fifo(min).map_err(|e| {
            log::error!("fatal error occurred while shrinking RocksDb: {}", e);
            ()
        })
    }
}
