/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: the on-disk store of a profile, in place of SQLite (`libsqlite3-sys`, C).
//!
//! One [`Store`] is one redb file. Servo kept one SQLite file per use (the client storage registry,
//! one per origin for localStorage, one per IndexedDB database, the HTTP disk cache), and so does
//! this: each of them opens one `Store` at the same place, with `.redb` in place of `.sqlite`.
//!
//! What Servo's SQLite settings promised, and what this keeps (see `rules.rs`):
//! - Servo ran SQLite in WAL mode with `synchronous = NORMAL`: every commit is atomic and the file
//!   is never left torn; a crash of the process loses nothing, a power cut may lose the last
//!   commits. Here [`Store::write`] is a non-durable redb commit (atomic, never torn, may roll
//!   back), and one thread makes all of them durable at most [`FLUSH_DELAY`] later, and on drop.
//!   A non-durable commit lives in the process's memory until then, so **a crash of the process
//!   as well as a power cut loses at most the last [`FLUSH_DELAY`] of `write`s**: weaker than
//!   Servo's SQLite for a process crash, never a half-applied commit. What must survive a crash
//!   uses [`Store::write_durable`] or [`Store::flush`] (IndexedDB does at the end of each
//!   read-write transaction, before `complete` fires; the registry for databases and persistence).
//!   localStorage keeps the window, as Chromium's batched localStorage commits do.
//! - Durable commits are redb's two-phase commits with quick repair: the file is web content, so
//!   redb's advice for malicious data applies, and reopening after a power cut on Juno is instant
//!   instead of a walk over the whole file.
//! - A redb file can be open once per process. SQLite let Servo open a connection per
//!   IndexedDB transaction; here callers share one `Store` (`Arc<Store>`).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

use log::warn;
pub use redb;
use redb::{Database, Durability, ReadTransaction, ReadableDatabase, WriteTransaction};

pub mod record;
pub mod rules;

/// How long a non-durable commit may wait for the commit that makes it durable.
pub const FLUSH_DELAY: Duration = Duration::from_millis(1000);

/// The page cache of one file. redb's default is 1 GiB; Servo set SQLite's to 2000 pages (8 MiB).
const CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Why the store failed.
#[derive(Debug)]
pub enum StoreError {
    /// redb's own error.
    Redb(redb::Error),
    /// A file or directory around the store.
    Io(io::Error),
    /// A record that does not decode (see `record.rs`), or a value of the wrong shape.
    Corrupt(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Redb(error) => write!(f, "store: {error}"),
            StoreError::Io(error) => write!(f, "store: {error}"),
            StoreError::Corrupt(what) => write!(f, "store: corrupt record: {what}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Redb(error) => Some(error),
            StoreError::Io(error) => Some(error),
            StoreError::Corrupt(_) => None,
        }
    }
}

impl StoreError {
    /// Whether the disk was full: Servo's `is_sqlite_disk_full_error`, which IndexedDB turns into
    /// a `QuotaExceededError`.
    pub fn is_disk_full(&self) -> bool {
        fn full(error: &io::Error) -> bool {
            error.kind() == io::ErrorKind::StorageFull || error.raw_os_error() == Some(28)
        }
        // redb's errors hold their io::Error in a variant but give no `source()`, so look inside.
        let io_error = match self {
            StoreError::Io(error) => Some(error),
            StoreError::Redb(redb::Error::Io(error)) => Some(error),
            _ => None,
        };
        if io_error.is_some_and(full) {
            return true;
        }
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(error) = source {
            if error.downcast_ref::<io::Error>().is_some_and(full) {
                return true;
            }
            source = error.source();
        }
        false
    }

    /// A record that did not decode.
    pub fn corrupt(what: impl Into<String>) -> Self {
        StoreError::Corrupt(what.into())
    }
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for StoreError {
            fn from(error: $t) -> Self {
                StoreError::Redb(error.into())
            }
        }
    )*};
}
from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError
);

impl From<redb::Error> for StoreError {
    fn from(error: redb::Error) -> Self {
        StoreError::Redb(error)
    }
}

impl From<io::Error> for StoreError {
    fn from(error: io::Error) -> Self {
        StoreError::Io(error)
    }
}

struct Inner {
    db: Database,
    /// A commit since the last durable one.
    dirty: AtomicBool,
    /// Whether this store has a file (an in-memory one has nothing to flush).
    on_disk: bool,
    /// Held for the whole of a flush: a second flush waits for the first to be on disk instead of
    /// finding `dirty` already cleared and returning early.
    flushing: Mutex<()>,
    /// A file this store created whose name, and the names above it, are not durable yet: see
    /// [`Store::open`]. Taken by the first flush that syncs them, put back if that fails.
    new_name: Mutex<Option<PathBuf>>,
}

impl Inner {
    fn new(db: Database, on_disk: bool, new_name: Option<PathBuf>) -> Inner {
        Inner {
            db,
            dirty: AtomicBool::new(false),
            on_disk,
            flushing: Mutex::new(()),
            new_name: Mutex::new(new_name),
        }
    }

    fn flush(&self) -> Result<(), StoreError> {
        let _flushing = self.flushing.lock().unwrap_or_else(|e| e.into_inner());
        self.sync_new_name()?;
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let result = (|| {
            let mut txn = self.db.begin_write()?;
            durable(&mut txn);
            txn.commit()?;
            Ok(())
        })();
        if result.is_err() {
            self.dirty.store(true, Ordering::Release);
        }
        result
    }

    /// Makes a new file's name durable, if it is not yet: every directory up to the root, deepest
    /// first. Some were made just now, others by Servo or LucinaNet a moment before
    /// (clientstorage/, webstorage/<origin>/, the profile itself), none of them synced. Only the
    /// file's own directory must succeed; one higher up that cannot be opened is skipped.
    fn sync_new_name(&self) -> Result<(), StoreError> {
        let mut new_name = self.new_name.lock().unwrap_or_else(|e| e.into_inner());
        let Some(path) = new_name.as_deref() else {
            return Ok(());
        };
        let mut dir = path.parent();
        let mut first = true;
        while let Some(d) = dir {
            match sync_dir(d) {
                Ok(()) => {},
                Err(error) if first => return Err(error),
                Err(_) => {},
            }
            first = false;
            dir = d.parent();
        }
        *new_name = None;
        Ok(())
    }
}

/// One redb file, or one in memory.
pub struct Store {
    inner: Arc<Inner>,
    path: Option<PathBuf>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish()
    }
}

fn durable(txn: &mut WriteTransaction) {
    let _ = txn.set_durability(Durability::Immediate);
    txn.set_quick_repair(true);
}

impl Store {
    /// Opens the file at `path`, or creates it and its directories.
    ///
    /// A new file's name, and the names of the directories above it, are made durable by syncing
    /// each directory up to the root: POSIX promises a new name only then. Linux's ext4 happens to commit the name with
    /// the file's own fsync; Juno's ext4 does not, and on QEMU power cuts every store created on
    /// `/data` came back missing (2026-11-04).
    ///
    /// Those syncs are the flusher's, like the commits it makes durable: within [`FLUSH_DELAY`],
    /// or at once by [`Store::flush`] and [`Store::write_durable`]. Each is a journal commit on
    /// Juno's `/data`, and the HTTP disk cache opens its store on the way to the first response,
    /// so syncing them here held a new profile's first page (example.com 0.3 s to 1.1 s, 2026-11-05).
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existed = path.exists();
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .create(path)?;
        let new_name = (!existed).then(|| path.to_owned());
        let inner = Arc::new(Inner::new(db, true, new_name));
        if !existed {
            flusher().wake(Arc::downgrade(&inner));
        }
        Ok(Store {
            inner,
            path: Some(path.to_owned()),
        })
    }

    /// A store on another redb backend, kept as a file would be (flushed): for the power-cut tests.
    #[doc(hidden)]
    pub fn with_backend(backend: impl redb::StorageBackend) -> Result<Store, StoreError> {
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .create_with_backend(backend)?;
        Ok(Store {
            inner: Arc::new(Inner::new(db, true, None)),
            path: None,
        })
    }

    /// A store that lives in memory only: Servo's `Connection::open_in_memory`.
    pub fn memory() -> Result<Store, StoreError> {
        let db = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .create_with_backend(redb::backends::InMemoryBackend::new())?;
        Ok(Store {
            inner: Arc::new(Inner::new(db, false, None)),
            path: None,
        })
    }

    /// Where the file is, if there is one.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// A read transaction: a snapshot of the last commit.
    pub fn read(&self) -> Result<ReadTransaction, StoreError> {
        Ok(self.inner.db.begin_read()?)
    }

    /// Runs `f` in one write transaction and commits it if `f` returns `Ok`, or drops it (rolls it
    /// back) if `f` returns `Err`. Every SQLite transaction of Servo's is one call of this.
    ///
    /// The commit is atomic at once and durable within [`FLUSH_DELAY`].
    pub fn write<T>(
        &self,
        f: impl FnOnce(&WriteTransaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut txn = self.inner.db.begin_write()?;
        if self.inner.on_disk && txn.set_durability(Durability::None).is_err() {
            durable(&mut txn);
        }
        let value = f(&txn)?;
        txn.commit()?;
        if self.inner.on_disk {
            self.inner.dirty.store(true, Ordering::Release);
            flusher().wake(Arc::downgrade(&self.inner));
        }
        Ok(value)
    }

    /// As [`Store::write`], but durable when it returns: for what must be on disk before
    /// something outside the file depends on it.
    pub fn write_durable<T>(
        &self,
        f: impl FnOnce(&WriteTransaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut txn = self.inner.db.begin_write()?;
        durable(&mut txn);
        let value = f(&txn)?;
        txn.commit()?;
        // A durable commit makes every earlier one durable too.
        self.inner.dirty.store(false, Ordering::Release);
        // And the file is found again only once its name is durable.
        self.inner.sync_new_name()?;
        Ok(value)
    }

    /// Makes every commit so far durable now.
    pub fn flush(&self) -> Result<(), StoreError> {
        self.inner.flush()
    }

    /// The file's size on disk, for storage estimates: 0 for one in memory.
    pub fn disk_size(&self) -> u64 {
        self.path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.inner.on_disk {
            if let Err(error) = self.inner.flush() {
                warn!("profile store {:?}: last flush failed: {error}", self.path);
            }
        }
    }
}

/// The one thread that makes non-durable commits durable, for every store in the process.
struct Flusher {
    queue: Mutex<Vec<Weak<Inner>>>,
    wake: Condvar,
}

fn flusher() -> &'static Flusher {
    static FLUSHER: OnceLock<&'static Flusher> = OnceLock::new();
    FLUSHER.get_or_init(|| {
        let flusher: &'static Flusher = Box::leak(Box::new(Flusher {
            queue: Mutex::new(Vec::new()),
            wake: Condvar::new(),
        }));
        let spawned = std::thread::Builder::new()
            .name("ProfileStoreFlush".to_owned())
            .spawn(move || flusher.run());
        if let Err(error) = spawned {
            warn!("profile store: no flush thread ({error}); commits become durable on drop only");
        }
        flusher
    })
}

impl Flusher {
    fn wake(&self, store: Weak<Inner>) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if !queue.iter().any(|queued| queued.ptr_eq(&store)) {
            queue.push(store);
        }
        self.wake.notify_one();
    }

    fn run(&self) {
        loop {
            {
                let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                while queue.is_empty() {
                    queue = self.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
                }
            }
            // Gather what else is written in this window, then make all of it durable at once.
            std::thread::sleep(FLUSH_DELAY / 2);
            let stores = std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()));
            for store in stores {
                let Some(store) = store.upgrade() else {
                    continue;
                };
                if let Err(error) = store.flush() {
                    warn!("profile store: flush failed: {error}");
                }
            }
        }
    }
}

/// Makes the names in `dir` durable. An empty path is the current directory.
fn sync_dir(dir: &Path) -> Result<(), StoreError> {
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(store: &Store) -> bool {
        store.inner.new_name.lock().unwrap().is_some()
    }

    #[test]
    fn a_new_file_leaves_its_name_to_the_flusher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/store.redb");
        let store = Store::open(&path).unwrap();
        assert!(pending(&store), "open synced the directories itself");
        store.flush().unwrap();
        assert!(!pending(&store));
        drop(store);
        // An existing file has nothing to sync.
        let store = Store::open(&path).unwrap();
        assert!(!pending(&store));
    }

    #[test]
    fn a_durable_write_makes_the_name_durable_too() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.redb")).unwrap();
        store.write_durable(|_| Ok(())).unwrap();
        assert!(!pending(&store));
    }

    #[test]
    fn the_flusher_syncs_a_new_name_without_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.redb")).unwrap();
        let deadline = std::time::Instant::now() + FLUSH_DELAY * 3;
        while pending(&store) {
            assert!(
                std::time::Instant::now() < deadline,
                "no flush within 3 × FLUSH_DELAY"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_missing_directory_fails_the_flush_and_stays_pending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/store.redb");
        let store = Store::open(&path).unwrap();
        *store.inner.new_name.lock().unwrap() = Some(dir.path().join("gone/store.redb"));
        assert!(store.flush().is_err());
        assert!(pending(&store));
    }
}
