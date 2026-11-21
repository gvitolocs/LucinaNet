/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: what the storage engines share around the profile store (redb), in place of
//! Servo's SQLite pragmas and `is_sqlite_disk_full_error` (now `StoreError::is_disk_full`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use profile_store::redb::{self, WriteTransaction};
use profile_store::{Store, StoreError};

/// How long an open waits for a file that is still being closed: the profile store's flush
/// thread may hold a store for the length of one commit after its last user dropped it.
const OPEN_WAIT: Duration = Duration::from_secs(3);

/// Opens the store at `path`, or returns the one already open there.
///
/// Servo opened one SQLite connection per user of a file: both storage thread groups (private and
/// public) opened the same `reg.sqlite` and the same `webstorage.sqlite` of an origin, and a
/// restart opened them again before the old threads were gone. A redb file may be open once per
/// process, so every user of a path shares one `Store` here, which sees every commit at once as
/// the SQLite connections did. `init` runs once per open of the file, before anyone else gets the
/// store (it creates the tables, and may clean up after a crash).
pub(crate) fn open_shared(
    path: &Path,
    init: impl FnOnce(&Store) -> Result<(), StoreError>,
) -> Result<Arc<Store>, StoreError> {
    // One slot per path: the map's lock is held only to find the slot, and the slot's while its
    // file is opened and `init` runs, so that a slow open (or a registry's sweep of its
    // directories) holds up the other users of that file only, and a file is never opened twice.
    type Slot = Arc<Mutex<Weak<Store>>>;
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, Slot>>> = OnceLock::new();
    let slot = {
        let mut open = OPEN
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Forget the slots of closed files, but not one that a caller holds or is filling.
        open.retain(|_, slot| {
            Arc::strong_count(slot) > 1 ||
                slot.try_lock()
                    .map_or(true, |store| store.strong_count() > 0)
        });
        open.entry(path.to_owned()).or_default().clone()
    };
    let mut current = slot.lock().unwrap_or_else(|error| error.into_inner());
    // A file deleted while open (localStorage cleared for a site) is not the file at `path`
    // any more: SQLite opened the new one, and so does this.
    if let Some(store) = current.upgrade() &&
        path.exists()
    {
        return Ok(store);
    }
    let store = Arc::new(open_waiting(path)?);
    init(&store)?;
    *current = Arc::downgrade(&store);
    Ok(store)
}

fn open_waiting(path: &Path) -> Result<Store, StoreError> {
    let start = Instant::now();
    loop {
        match Store::open(path) {
            Err(StoreError::Redb(redb::Error::DatabaseAlreadyOpen))
                if start.elapsed() < OPEN_WAIT =>
            {
                std::thread::sleep(Duration::from_millis(10));
            },
            result => return result,
        }
    }
}

/// Whether a commit must be on disk when it returns (`Store::write_durable`) or within the
/// profile store's flush delay (`Store::write`).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Commit {
    Soon,
    Durable,
}

/// Runs `f` in one write transaction: committed if it returns `Ok`, rolled back if it returns
/// `Err`, as a SQLite transaction that Servo dropped on an error. The outer error is the store's
/// own (beginning or committing the transaction); the inner one is `f`'s.
pub(crate) fn write_or_roll_back<T, E>(
    store: &Store,
    commit: Commit,
    f: impl FnOnce(&WriteTransaction) -> Result<T, E>,
) -> Result<Result<T, E>, StoreError> {
    let mut failure = None;
    let body = |txn: &WriteTransaction| match f(txn) {
        Ok(value) => Ok(value),
        Err(error) => {
            failure = Some(error);
            Err(StoreError::corrupt("rolled back"))
        },
    };
    let result = match commit {
        Commit::Soon => store.write(body),
        Commit::Durable => store.write_durable(body),
    };
    match (result, failure) {
        (_, Some(error)) => Ok(Err(error)),
        (Ok(value), None) => Ok(Ok(value)),
        (Err(error), None) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::open_shared;

    #[test]
    fn one_store_and_one_init_per_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.redb");
        let inits = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let inits = inits.clone();
                std::thread::spawn(move || {
                    open_shared(&path, |_| {
                        inits.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(())
                    })
                    .unwrap()
                })
            })
            .collect();
        let stores: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(inits.load(Ordering::SeqCst), 1);
        assert!(stores.iter().all(|store| Arc::ptr_eq(store, &stores[0])));
    }

    #[test]
    fn a_slow_init_holds_up_its_own_path_only() {
        let dir = tempfile::tempdir().unwrap();
        let slow = dir.path().join("slow.redb");
        let other = dir.path().join("other.redb");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let slow_open = std::thread::spawn(move || {
            open_shared(&slow, |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
            .unwrap()
        });
        started_rx.recv().unwrap();
        // While the slow init runs, another file opens.
        let (done_tx, done_rx) = mpsc::channel();
        let other_open = std::thread::spawn(move || {
            let store = open_shared(&other, |_| Ok(())).unwrap();
            done_tx.send(()).unwrap();
            store
        });
        assert!(done_rx.recv_timeout(Duration::from_secs(5)).is_ok());
        release_tx.send(()).unwrap();
        slow_open.join().unwrap();
        other_open.join().unwrap();
    }
}
