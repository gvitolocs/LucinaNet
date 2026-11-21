/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: localStorage on the profile store (redb), in place of Servo's SQLite engine. One
//! file per origin, `<origin dir>/webstorage.redb` (Servo: `webstorage.sqlite`).

use std::path::PathBuf;
use std::sync::Arc;

use log::error;
use profile_store::redb::{ReadableTable, TableDefinition};
use profile_store::{Store, StoreError};
use servo_base::threadpool::ThreadPool;

use crate::shared::open_shared;
use crate::webstorage::OriginEntry;
use crate::webstorage::engines::WebStorageEngine;

/// key → value. Servo's table had an autoincrement id and no unique key on `key`; `set` was
/// update-or-insert and `load` returned every pair into a map, so a table keyed by `key` is the
/// same. Both are kept as bytes and checked as UTF-8 when read: redb's own `&str` panics on bytes
/// that are not UTF-8, and a corrupt file must give an error.
const DATA: TableDefinition<&[u8], &[u8]> = TableDefinition::new("data");

fn text(bytes: &[u8]) -> Result<String, StoreError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| StoreError::corrupt("localStorage text that is not UTF-8"))
}

fn create_table(store: &Store) -> Result<(), StoreError> {
    store.write(|txn| {
        txn.open_table(DATA)?;
        Ok(())
    })
}

pub struct RedbEngine {
    store: Arc<Store>,
}

impl RedbEngine {
    pub fn new(db_dir: &Option<PathBuf>, _pool: Arc<ThreadPool>) -> Result<Self, StoreError> {
        let store = match db_dir {
            Some(path) => open_shared(&path.join("webstorage.redb"), create_table)?,
            None => {
                // We probably don't need an in memory implementation at all.
                // WebStorageEnvironment already keeps all key value pairs in memory via its data field.
                // A future refactoring could avoid creating a WebStorageEngine entirely when config_dir is None.
                let store = Store::memory()?;
                create_table(&store)?;
                Arc::new(store)
            },
        };
        Ok(RedbEngine { store })
    }
}

impl WebStorageEngine for RedbEngine {
    type Error = StoreError;

    fn load(&self) -> Result<OriginEntry, Self::Error> {
        let txn = self.store.read()?;
        let table = txn.open_table(DATA)?;
        let mut map = OriginEntry::default();
        for row in table.iter()? {
            let (key, value) = row?;
            map.insert(text(key.value())?, text(value.value())?);
        }
        Ok(map)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.store.write(|txn| {
            txn.open_table(DATA)?.retain(|_, _| false)?;
            Ok(())
        })
    }

    fn delete(&mut self, key: &str) -> Result<(), Self::Error> {
        self.store.write(|txn| {
            txn.open_table(DATA)?.remove(key.as_bytes())?;
            Ok(())
        })
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), Self::Error> {
        // update or insert
        self.store.write(|txn| {
            txn.open_table(DATA)?
                .insert(key.as_bytes(), value.as_bytes())?;
            Ok(())
        })
    }

    fn save(&mut self, data: &OriginEntry) {
        let result = self.store.write(|txn| {
            let mut table = txn.open_table(DATA)?;
            table.retain(|_, _| false)?;
            for (key, value) in data.inner() {
                table.insert(key.as_bytes(), value.as_bytes())?;
            }
            Ok(())
        });
        if let Err(e) = result {
            error!("localstorage save error: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use servo_base::threadpool::ThreadPool;

    use super::*;

    fn pairs(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn restart_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = Some(dir.path().to_path_buf());
        let mut engine = RedbEngine::new(&path, ThreadPool::global()).unwrap();
        engine.set("a", "1").unwrap();
        engine.set("b", "2").unwrap();
        engine.set("a", "3").unwrap();
        engine.delete("b").unwrap();
        drop(engine);

        let engine = RedbEngine::new(&path, ThreadPool::global()).unwrap();
        assert_eq!(engine.load().unwrap().inner(), &pairs(&[("a", "3")]));
    }

    #[test]
    fn save_replaces_everything() {
        let mut engine = RedbEngine::new(&None, ThreadPool::global()).unwrap();
        engine.set("x", "1").unwrap();
        let mut entry = OriginEntry::default();
        entry.insert("y".to_owned(), "2".to_owned());
        engine.save(&entry);
        assert_eq!(engine.load().unwrap().inner(), &pairs(&[("y", "2")]));
    }

    #[test]
    fn clear_empties() {
        let mut engine = RedbEngine::new(&None, ThreadPool::global()).unwrap();
        engine.set("a", "1").unwrap();
        engine.set("b", "2").unwrap();
        engine.clear().unwrap();
        assert!(engine.load().unwrap().inner().is_empty());
    }

    #[test]
    fn corrupt_text_is_an_error() {
        let engine = RedbEngine::new(&None, ThreadPool::global()).unwrap();
        engine
            .store
            .write(|txn| {
                txn.open_table(DATA)?
                    .insert(&b"key"[..], &[0xff, 0xfe][..])?;
                Ok(())
            })
            .unwrap();
        assert!(engine.load().is_err());
    }
}
