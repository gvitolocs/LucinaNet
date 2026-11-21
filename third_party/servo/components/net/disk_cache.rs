/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use log::error;
use malloc_size_of_derive::MallocSizeOf;
use profile_store::record::{Reader, Writer};
use profile_store::redb::{ReadableTable, Table, TableDefinition};
use profile_store::rules::evict::evict;
use profile_store::{Store, StoreError};
use servo_config::pref;
use servo_url::ServoUrl;
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};

use crate::http_cache::{
    CacheEntry, CacheKey, CachedResource, HttpCacheAssignment, MemoryCacheLifecycle,
};

/// The cache rows: cache key -> record (data bytes, size u64, insertion_timestamp u64).
const DISK_CACHE_TABLE: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("disk_cache");
/// Insertion order: (insertion_seq, cache key) -> (), so entries load oldest first.
/// LucinaNet: Servo had no such table; it relied on SQLite rowid being the insertion order.
const DISK_CACHE_ORDER_TABLE: TableDefinition<'static, (u64, &str), ()> =
    TableDefinition::new("disk_cache_order");
/// Each key's insertion_seq, so its order row is found without a scan.
const DISK_CACHE_SEQ_TABLE: TableDefinition<'static, &str, u64> =
    TableDefinition::new("disk_cache_seq");
/// Counters, e.g. the next insertion_seq.
const META_TABLE: TableDefinition<'static, &str, u64> = TableDefinition::new("meta");
const NEXT_SEQ_KEY: &str = "next_seq";

#[derive(MallocSizeOf)]
pub(crate) struct DiskCacheMetadata {
    key: CacheKey,
    /// The size of the serialization or the exact size from the cache metadata.
    size: usize,
}

/// Encodes one `disk_cache` row value.
fn encode_row(data: &[u8], size: u64, insertion_timestamp: u64) -> Result<Vec<u8>, StoreError> {
    Ok(Writer::new()
        .bytes(data)?
        .u64(size)?
        .u64(insertion_timestamp)?
        .finish())
}

/// Decodes one `disk_cache` row value.
fn decode_row(bytes: &[u8]) -> Result<(Vec<u8>, u64, u64), StoreError> {
    let mut reader = Reader::new(bytes);
    let data = reader.bytes()?.to_vec();
    let size = reader.u64()?;
    let insertion_timestamp = reader.u64()?;
    reader.end()?;
    Ok((data, size, insertion_timestamp))
}

/// Removes the order row of `key`, found through its seq.
fn remove_order_rows(
    order: &mut Table<'_, (u64, &str), ()>,
    seqs: &mut Table<'_, &str, u64>,
    key: &str,
) -> Result<(), StoreError> {
    let seq = seqs.remove(key)?.map(|guard| guard.value());
    if let Some(seq) = seq {
        order.remove((seq, key))?;
    }
    Ok(())
}

/// This data structure will be per [`HttpCacheAssignment`]. Currently, we only store HttpCacheAssignment::Public and otherwise return zero.
/// As this data structure stores the state of the cache, if other instances share the same database, special care has to be taken
/// to ensure conistency.
#[derive(MallocSizeOf)]
struct DiskCacheInner {
    entries: VecDeque<DiskCacheMetadata>,
    size: usize,
    #[ignore_malloc_size_of = "Find a better way"]
    db: Store,
    cache_assignment: HttpCacheAssignment,
}

#[derive(MallocSizeOf)]
/// A struct representing the disk cache.
pub(crate) struct DiskCache {
    path: PathBuf,
    max_size: usize,
    // the non constant data.
    inner: TokioMutex<DiskCacheInner>,
}

/// Get the storage path out of `network_http_disk_cache` preference and `temporary_storage` option.
fn storage_dir() -> Option<PathBuf> {
    let disk_storage_path = pref!(network_http_disk_cache);
    match (
        servo_config::opts::get().temporary_storage,
        disk_storage_path.is_empty(),
    ) {
        (false, false) => Some(disk_storage_path.into()),
        (true, true) => {
            let tmp_dir = tempfile::tempdir().unwrap();
            let mut path = tmp_dir.path().to_path_buf();
            // LucinaNet: the temporary file is cache.redb, was cache.sqlite3.
            path.set_file_name("cache.redb");
            Some(path)
        },
        (true, false) => {
            error!(
                "Temporary storage cannot be set with explicit disk storage path. Disabling http_disk_cache"
            );
            None
        },
        (false, true) => None,
    }
}

/// Loads the entries oldest first (Servo loaded `SELECT key, size` in rowid = insertion order).
fn load_entries(db: &Store) -> Result<VecDeque<DiskCacheMetadata>, StoreError> {
    let read = db.read()?;
    let order = read.open_table(DISK_CACHE_ORDER_TABLE)?;
    let cache = read.open_table(DISK_CACHE_TABLE)?;
    let mut entries = VecDeque::new();
    for row in order.range::<(u64, &str)>(..)? {
        let (order_key, _) = row?;
        let (_seq, key) = order_key.value();
        let Some(row) = cache.get(key)? else { continue };
        match decode_row(row.value()) {
            Ok((_data, size, _timestamp)) => {
                // LucinaNet: Servo unwrapped the URL parse; a key read from disk that does not
                // parse is skipped instead of panicking.
                match ServoUrl::parse(key) {
                    Ok(url) => entries.push_back(DiskCacheMetadata {
                        key: CacheKey::from_url(url),
                        size: size as usize,
                    }),
                    Err(e) => error!("Could not parse cached key {key}: {e}"),
                }
            },
            Err(e) => error!("Could not decode cached row for {key}: {e}"),
        }
    }
    Ok(entries)
}

impl DiskCache {
    /// Creates a new [`DiskCache`] if the preference if set.
    /// Creates the tables if they do not exist and opens the store.
    pub(crate) fn new(
        cache_assignment: HttpCacheAssignment,
    ) -> (Option<Arc<DiskCache>>, MemoryCacheLifecycle) {
        // For private browsing we currently do not want to store any disk cache.
        let disk_cache_path = storage_dir();

        if let Some(disk_cache_path) = disk_cache_path
            && cache_assignment == HttpCacheAssignment::Public
        {
            let Ok(max_disk_cache_size) = pref!(network_http_disk_cache_size).try_into() else {
                return (None, MemoryCacheLifecycle::empty());
            };

            let Ok(db) = Store::open(&disk_cache_path) else {
                error!("Could not open disk cache database");
                return (None, MemoryCacheLifecycle::empty());
            };

            if let Err(e) = db.write(|txn| {
                txn.open_table(DISK_CACHE_TABLE)?;
                txn.open_table(DISK_CACHE_ORDER_TABLE)?;
                txn.open_table(DISK_CACHE_SEQ_TABLE)?;
                txn.open_table(META_TABLE)?;
                Ok(())
            }) {
                error!("Could not create tables. Store error {:?}", e);
                return (None, MemoryCacheLifecycle::empty());
            }

            let entries = match load_entries(&db) {
                Ok(entries) => entries,
                Err(e) => {
                    error!("Could not get disk data. Store error {:?}", e);
                    return (None, MemoryCacheLifecycle::empty());
                },
            };
            let size = entries.iter().map(|entry| entry.size).sum();
            let inner = DiskCacheInner {
                entries,
                size,
                db,
                cache_assignment,
            };
            let disk_cache_data = std::sync::Arc::new(DiskCache {
                inner: TokioMutex::new(inner),
                path: disk_cache_path,
                max_size: max_disk_cache_size,
            });

            (
                Some(disk_cache_data.clone()),
                MemoryCacheLifecycle {
                    disk_cache: Some(disk_cache_data),
                },
            )
        } else {
            (None, MemoryCacheLifecycle::empty())
        }
    }

    /// Restores a cache entry from the disk if it exists.
    /// Deletes the entry from the disk cache
    #[servo_tracing::instrument(skip(self))]
    pub(crate) async fn get(&self, key: CacheKey) -> Option<Arc<TokioRwLock<Vec<CachedResource>>>> {
        let bytes = {
            // we lock the metadata before we update the database so that
            // the database and metadata are consistent when this lock is released.
            let mut inner = self.inner.lock().await;
            let _span = profile_traits::trace_span!("query disk cache").entered();
            let result = inner.db.write(|txn| {
                let mut cache = txn.open_table(DISK_CACHE_TABLE)?;
                let decoded = match cache.get(key.as_ref())? {
                    Some(row) => decode_row(row.value()),
                    None => return Ok(None),
                };
                // The row goes either way, as in Servo's read-then-delete.
                cache.remove(key.as_ref())?;
                let mut order = txn.open_table(DISK_CACHE_ORDER_TABLE)?;
                let mut seqs = txn.open_table(DISK_CACHE_SEQ_TABLE)?;
                remove_order_rows(&mut order, &mut seqs, key.as_ref())?;
                match decoded {
                    Ok((data, _size, _timestamp)) => Ok(Some(data)),
                    // LucinaNet: Servo's query failed on an undecodable row; here the row is
                    // deleted and None returned, never a panic.
                    Err(e) => {
                        error!("Could not decode cached data for {}: {}", key.as_ref(), e);
                        Ok(None)
                    },
                }
            });

            match result {
                Ok(bytes) => {
                    // update the metadata; size stays the sum of entries (Servo recomputed it
                    // from a SQL SUM).
                    if let Some(entry_index) = inner
                        .entries
                        .iter()
                        .position(|metadata| metadata.key == key)
                    {
                        let entry = inner.entries.remove(entry_index);
                        if let Some(entry) = entry {
                            inner.size -= entry.size;
                        }
                    }
                    bytes
                },
                Err(e) => {
                    error!("Could not read cached data from disk: {}", e);
                    None
                },
            }
        };
        let Some(bytes) = bytes else {
            return None;
        };
        let _span = profile_traits::trace_span!("deserialize cache request").entered();
        let Ok(value) = postcard::from_bytes(&bytes) else {
            error!("Could not deserialize cached resource");
            return None;
        };
        let deserialized_vec_cached_response = std::sync::Arc::new(TokioRwLock::new(value));

        Some(deserialized_vec_cached_response)
    }

    /// Stores a [`CacheEntry`]` to disk.
    #[servo_tracing::instrument(skip(self))]
    pub(crate) async fn store(&self, key: CacheKey, entry: CacheEntry) {
        let entry = entry.read().await;
        let data_to_serialize: Vec<&CachedResource> = entry
            .iter()
            .filter(|cached_resource| cached_resource.is_done())
            .collect();
        let Ok(data) = postcard::to_stdvec(&*data_to_serialize) else {
            error!("Could not deserialize value");
            return;
        };

        {
            let mut inner = self.inner.lock().await;
            let data_size = data.len();

            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let result = inner.db.write(|txn| {
                let mut cache = txn.open_table(DISK_CACHE_TABLE)?;
                let mut order = txn.open_table(DISK_CACHE_ORDER_TABLE)?;
                let mut seqs = txn.open_table(DISK_CACHE_SEQ_TABLE)?;
                let mut meta = txn.open_table(META_TABLE)?;
                let seq = match meta.get(NEXT_SEQ_KEY)? {
                    Some(guard) => guard.value(),
                    None => 0,
                };
                let record = encode_row(&data, data_size as u64, timestamp)?;
                // insert or replace, as Servo's ON CONFLICT update did
                cache.insert(key.as_ref(), record.as_slice())?;
                remove_order_rows(&mut order, &mut seqs, key.as_ref())?;
                order.insert((seq, key.as_ref()), ())?;
                seqs.insert(key.as_ref(), seq)?;
                meta.insert(NEXT_SEQ_KEY, seq + 1)?;
                Ok(())
            });
            if let Err(e) = result {
                error!("Could not insert cache data. Error {}", e);
            }
            // LucinaNet: Servo pushed a duplicate entry when the key was already present and
            // recomputed size from a SQL SUM; here the old entry is removed first and size
            // stays the sum of entries.
            if let Some(entry_index) = inner
                .entries
                .iter()
                .position(|metadata| metadata.key == key)
            {
                inner.entries.remove(entry_index);
            }
            inner.entries.push_back(DiskCacheMetadata {
                key,
                size: data_size,
            });
            inner.size = inner.entries.iter().map(|entry| entry.size).sum();
        }
        self.delete_until_cache_size().await;
    }

    /// Deletes data from the cache until the size is <= max_size
    #[servo_tracing::instrument(skip(self))]
    async fn delete_until_cache_size(&self) {
        let mut inner = self.inner.lock().await;
        let sizes_oldest_first: Vec<u64> = inner
            .entries
            .iter()
            .map(|entry| entry.size as u64)
            .collect();
        let (drop_count, _left) =
            evict(&sizes_oldest_first, inner.size as u64, self.max_size as u64);
        if drop_count == 0 {
            return;
        }
        // LucinaNet: Servo popped from the back (the newest, what was just stored), could loop
        // forever when entries ran out with size still over max, and could underflow size;
        // evict() drops from the front (the oldest) and always terminates.
        let dropped: Vec<CacheKey> = inner
            .entries
            .drain(..drop_count)
            .map(|metadata| metadata.key)
            .collect();

        let result = inner.db.write(|txn| {
            let mut cache = txn.open_table(DISK_CACHE_TABLE)?;
            let mut order = txn.open_table(DISK_CACHE_ORDER_TABLE)?;
            let mut seqs = txn.open_table(DISK_CACHE_SEQ_TABLE)?;
            for key in &dropped {
                cache.remove(key.as_ref())?;
                remove_order_rows(&mut order, &mut seqs, key.as_ref())?;
            }
            Ok(())
        });
        if let Err(e) = result {
            error!("Could not delete old disk cache entries: {}", e);
        }
        inner.size = inner.entries.iter().map(|entry| entry.size).sum();
    }

    /// Clears the disk cache.
    /// Should only be called in sync context and will panic.
    #[servo_tracing::instrument(skip(self))]
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.blocking_lock();
        let result = inner.db.write(|txn| {
            txn.open_table(DISK_CACHE_TABLE)?.retain(|_, _| false)?;
            txn.open_table(DISK_CACHE_ORDER_TABLE)?
                .retain(|_, _| false)?;
            txn.open_table(DISK_CACHE_SEQ_TABLE)?.retain(|_, _| false)?;
            Ok(())
        });
        if let Err(e) = result {
            error!("Could not clear disk cache: {}", e);
        }
        inner.entries.clear();
        inner.size = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache(dir: &tempfile::TempDir, max_size: usize) -> DiskCache {
        let path = dir.path().join("cache.redb");
        let db = Store::open(&path).unwrap();
        db.write(|txn| {
            txn.open_table(DISK_CACHE_TABLE)?;
            txn.open_table(DISK_CACHE_ORDER_TABLE)?;
            txn.open_table(META_TABLE)?;
            Ok(())
        })
        .unwrap();
        DiskCache {
            path,
            max_size,
            inner: TokioMutex::new(DiskCacheInner {
                entries: VecDeque::new(),
                size: 0,
                db,
                cache_assignment: HttpCacheAssignment::Public,
            }),
        }
    }

    fn key(url: &str) -> CacheKey {
        CacheKey::from_url(ServoUrl::parse(url).unwrap())
    }

    fn empty_entry() -> CacheEntry {
        Arc::new(TokioRwLock::new(Vec::new()))
    }

    async fn entry_keys(cache: &DiskCache) -> Vec<String> {
        cache
            .inner
            .lock()
            .await
            .entries
            .iter()
            .map(|metadata| metadata.key.as_ref().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn store_then_reopen_keeps_entries_and_their_order() {
        let dir = tempfile::tempdir().unwrap();
        let first = key("https://example.com/1");
        let second = key("https://example.com/2");
        {
            let cache = test_cache(&dir, 1024);
            cache.store(first.clone(), empty_entry()).await;
            cache.store(second.clone(), empty_entry()).await;
            assert_eq!(
                entry_keys(&cache).await,
                vec![first.as_ref(), second.as_ref()]
            );
        }
        // A redb file is open once per process, so the first cache is dropped before reopening.
        let db = Store::open(&dir.path().join("cache.redb")).unwrap();
        let entries = load_entries(&db).unwrap();
        let keys: Vec<&str> = entries.iter().map(|entry| entry.key.as_ref()).collect();
        assert_eq!(keys, vec![first.as_ref(), second.as_ref()]);
        let size: usize = entries.iter().map(|entry| entry.size).sum();
        assert!(size > 0);
    }

    #[tokio::test]
    async fn eviction_drops_the_oldest_first_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        // Each stored empty entry serializes to one byte, so three entries with max 2 evict one.
        let cache = test_cache(&dir, 2);
        let oldest = key("https://example.com/oldest");
        let middle = key("https://example.com/middle");
        let newest = key("https://example.com/newest");
        cache.store(oldest.clone(), empty_entry()).await;
        cache.store(middle.clone(), empty_entry()).await;
        cache.store(newest.clone(), empty_entry()).await;
        assert_eq!(
            entry_keys(&cache).await,
            vec![middle.as_ref(), newest.as_ref()]
        );
        assert_eq!(cache.inner.lock().await.size, 2);
        // The evicted row is gone from the store.
        assert!(cache.get(oldest).await.is_none());
    }

    #[tokio::test]
    async fn replacing_a_key_does_not_double_count_size() {
        let dir = tempfile::tempdir().unwrap();
        let cache = test_cache(&dir, 1024);
        let key = key("https://example.com/");
        cache.store(key.clone(), empty_entry()).await;
        let size_after_first = cache.inner.lock().await.size;
        cache.store(key.clone(), empty_entry()).await;
        let inner = cache.inner.lock().await;
        assert_eq!(inner.entries.len(), 1);
        assert_eq!(inner.size, size_after_first);
    }

    #[tokio::test]
    async fn a_corrupt_record_gives_none_and_removes_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let cache = test_cache(&dir, 1024);
        let key = key("https://example.com/");
        cache.store(key.clone(), empty_entry()).await;
        // Corrupt the row behind the cache's back.
        cache
            .inner
            .lock()
            .await
            .db
            .write(|txn| {
                let mut table = txn.open_table(DISK_CACHE_TABLE)?;
                table.insert(key.as_ref(), &b"not a record"[..])?;
                Ok(())
            })
            .unwrap();
        assert!(cache.get(key.clone()).await.is_none());
        // The row was removed, and so was the metadata entry.
        let inner = cache.inner.lock().await;
        assert!(inner.entries.is_empty());
        assert_eq!(inner.size, 0);
        let read = inner.db.read().unwrap();
        let table = read.open_table(DISK_CACHE_TABLE).unwrap();
        assert!(table.get(key.as_ref()).unwrap().is_none());
    }
}
