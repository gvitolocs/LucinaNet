/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! LucinaNet: the IndexedDB engine on the profile store (redb), in place of Servo's SQLite one.
//!
//! One file per database, `<path>/indexeddb.redb` (Servo: `indexeddb.sqlite`), with the tables of
//! Servo's schema (adapted from Firefox's `DBSchema.cpp`). Keys of `object_data` are the bytes of
//! `encoding.rs`, Firefox's order-preserving key encoding, which redb compares bytewise as SQLite
//! compares BLOBs: key ranges, the order of results and counts are the same. Servo created
//! `index_data` and `unique_index_data` but never wrote them, so they are not here.

use std::fmt;
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use log::{info, warn};
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use profile_store::record::{Reader, Writer};
use profile_store::redb::{ReadableTable, TableDefinition, TableHandle, WriteTransaction};
use profile_store::{Store, StoreError};
use profile_traits::generic_callback::GenericCallback;
use servo_base::threadpool::ThreadPool;
use storage_traits::indexeddb::{
    AsyncOperation, AsyncReadOnlyOperation, AsyncReadWriteOperation, AsyncSchemaOperation,
    BackendError, BackendResult, CreateObjectResult, IndexedDBIndex, IndexedDBKeyRange,
    IndexedDBKeyType, IndexedDBRecord, IndexedDBTxnMode, KeyPath, PutItemResult,
};

use crate::indexeddb::IndexedDBDescription;
use crate::indexeddb::engines::{KvsEngine, KvsTransaction, encoding};
use crate::shared::{Commit, open_shared, write_or_roll_back};

/// `database (name primary key, origin, version)`: name → (origin, version). One row.
const DATABASE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("database");
/// `object_store (id primary key autoincrement, name unique, key_path, auto_increment)`:
/// id → (name, key_path or NULL, auto_increment).
const OBJECT_STORE: TableDefinition<u64, &[u8]> = TableDefinition::new("object_store");
/// `name unique`: the name as SQLite kept it in its index → id. Servo's databases were UTF-16
/// (`PRAGMA encoding = 'UTF-16'`, little-endian), and SQLite compares text bytewise in the
/// database's encoding, so `SELECT name FROM object_store` (a scan of that index) returned the
/// names in the order of their UTF-16LE bytes: keyed so, they come out in the same order.
const OBJECT_STORE_BY_NAME: TableDefinition<&[u8], u64> =
    TableDefinition::new("object_store_by_name");
/// `object_data (object_store_id, key, data, primary key (object_store_id, key))`.
const OBJECT_DATA: TableDefinition<(u64, &[u8]), &[u8]> = TableDefinition::new("object_data");
/// `object_store_index (id, object_store_id, name, key_path, unique_index, multi_entry_index,
/// unique(object_store_id, name))`: (object_store_id, name in UTF-16LE) → (id, name, key_path,
/// unique_index, multi_entry_index). Servo's `WHERE object_store_id = ?` read that unique index,
/// so in this order.
const OBJECT_STORE_INDEX: TableDefinition<(u64, &[u8]), &[u8]> =
    TableDefinition::new("object_store_index");
/// The last id given out by each `autoincrement` id above (SQLite's `sqlite_sequence`).
const SEQUENCES: TableDefinition<&[u8], u64> = TableDefinition::new("sequences");

/// Why the engine failed: the store's error, or what rusqlite reported for the same case.
#[derive(Debug)]
pub enum IdbError {
    Store(StoreError),
    /// rusqlite's `QueryReturnedNoRows`.
    NoRow(&'static str),
    /// A UNIQUE constraint.
    Unique(&'static str),
    /// A key path that does not serialize.
    Serialize(String),
}

impl IdbError {
    /// Servo's `is_sqlite_disk_full_error`.
    pub fn is_disk_full(&self) -> bool {
        matches!(self, IdbError::Store(error) if error.is_disk_full())
    }
}

impl fmt::Display for IdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdbError::Store(error) => write!(f, "{error}"),
            IdbError::NoRow(table) => write!(f, "Query returned no rows ({table})"),
            IdbError::Unique(columns) => write!(f, "UNIQUE constraint failed: {columns}"),
            IdbError::Serialize(error) => write!(f, "cannot serialize: {error}"),
        }
    }
}

impl std::error::Error for IdbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IdbError::Store(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StoreError> for IdbError {
    fn from(error: StoreError) -> Self {
        IdbError::Store(error)
    }
}

macro_rules! idb_error_from {
    ($($t:ty),*) => {$(
        impl From<$t> for IdbError {
            fn from(error: $t) -> Self {
                IdbError::Store(error.into())
            }
        }
    )*};
}
idb_error_from!(
    profile_store::redb::TableError,
    profile_store::redb::StorageError,
    profile_store::redb::TransactionError
);

/// A name as SQLite compared it: its UTF-16LE bytes.
fn name_key(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn corrupt(what: impl Into<String>) -> IdbError {
    IdbError::Store(StoreError::corrupt(what))
}

/// A row of `object_store`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectStore {
    pub id: u64,
    pub name: String,
    pub key_path: Option<Vec<u8>>,
    pub auto_increment: i64,
}

impl ObjectStore {
    fn record(&self) -> Result<Vec<u8>, StoreError> {
        Ok(Writer::new()
            .str(&self.name)?
            .opt_bytes(self.key_path.as_deref())?
            .i64(self.auto_increment)?
            .finish())
    }

    fn read(id: u64, bytes: &[u8]) -> Result<ObjectStore, StoreError> {
        let mut reader = Reader::new(bytes);
        let store = ObjectStore {
            id,
            name: reader.string()?,
            key_path: reader.opt_bytes()?.map(<[u8]>::to_vec),
            auto_increment: reader.i64()?,
        };
        reader.end()?;
        Ok(store)
    }
}

/// A row of `object_store_index`.
struct Index {
    id: u64,
    name: String,
    key_path: Vec<u8>,
    unique: bool,
    multi_entry: bool,
}

impl Index {
    fn record(&self) -> Result<Vec<u8>, StoreError> {
        Ok(Writer::new()
            .u64(self.id)?
            .str(&self.name)?
            .bytes(&self.key_path)?
            .bool(self.unique)?
            .bool(self.multi_entry)?
            .finish())
    }

    fn read(bytes: &[u8]) -> Result<Index, StoreError> {
        let mut reader = Reader::new(bytes);
        let index = Index {
            id: reader.u64()?,
            name: reader.string()?,
            key_path: reader.bytes()?.to_vec(),
            unique: reader.bool()?,
            multi_entry: reader.bool()?,
        };
        reader.end()?;
        Ok(index)
    }
}

fn database_record(origin: &str, version: u64) -> Result<Vec<u8>, StoreError> {
    Ok(Writer::new().str(origin)?.u64(version)?.finish())
}

fn read_database(bytes: &[u8]) -> Result<(String, u64), StoreError> {
    let mut reader = Reader::new(bytes);
    let origin = reader.string()?;
    let version = reader.u64()?;
    reader.end()?;
    Ok((origin, version))
}

fn next_id(txn: &WriteTransaction, table: &'static str) -> Result<u64, IdbError> {
    let mut sequences = txn.open_table(SEQUENCES)?;
    let last = sequences
        .get(table.as_bytes())?
        .map(|last| last.value())
        .unwrap_or(0);
    let id = last
        .checked_add(1)
        .filter(|id| *id <= i64::MAX as u64)
        .ok_or_else(|| corrupt(format!("no id left in {table}")))?;
    sequences.insert(table.as_bytes(), id)?;
    Ok(id)
}

/// The object store named `name`: `SELECT * FROM object_store WHERE name = ?`.
fn find_store(
    names: &impl ReadableTable<&'static [u8], u64>,
    stores: &impl ReadableTable<u64, &'static [u8]>,
    name: &str,
) -> Result<Option<ObjectStore>, IdbError> {
    let Some(id) = names.get(name_key(name).as_slice())?.map(|id| id.value()) else {
        return Ok(None);
    };
    let record = stores
        .get(id)?
        .ok_or_else(|| corrupt(format!("object store {id} without its row")))?;
    Ok(Some(ObjectStore::read(id, record.value())?))
}

fn find_store_in_write(
    txn: &WriteTransaction,
    name: &str,
) -> Result<Option<ObjectStore>, IdbError> {
    find_store(
        &txn.open_table(OBJECT_STORE_BY_NAME)?,
        &txn.open_table(OBJECT_STORE)?,
        name,
    )
}

fn find_store_in_read(store: &Store, name: &str) -> Result<Option<ObjectStore>, IdbError> {
    let txn = store.read()?;
    find_store(
        &txn.open_table(OBJECT_STORE_BY_NAME)?,
        &txn.open_table(OBJECT_STORE)?,
        name,
    )
}

fn write_store(txn: &WriteTransaction, store: &ObjectStore) -> Result<(), IdbError> {
    txn.open_table(OBJECT_STORE)?
        .insert(store.id, store.record()?.as_slice())?;
    Ok(())
}

/// Servo's `range_to_query`: where the keys of a range start and end, encoded, and whether each
/// end is open. `None` is no bound.
fn range_bounds(range: &IndexedDBKeyRange) -> (Option<(Vec<u8>, bool)>, Option<(Vec<u8>, bool)>) {
    // Special case for optimization
    if let Some(singleton) = range.as_singleton() {
        let encoded = encoding::serialize(singleton);
        return (Some((encoded.clone(), false)), Some((encoded, false)));
    }
    (
        range
            .lower
            .as_ref()
            .map(|lower| (encoding::serialize(lower), range.lower_open)),
        range
            .upper
            .as_ref()
            .map(|upper| (encoding::serialize(upper), range.upper_open)),
    )
}

/// Calls `each` with every row of `object_data` of the store `store_id` whose key lies in
/// `range`, in key order, until it returns false.
fn for_each_in_range(
    data: &impl ReadableTable<(u64, &'static [u8]), &'static [u8]>,
    store_id: u64,
    range: &IndexedDBKeyRange,
    mut each: impl FnMut(&[u8], &[u8]) -> bool,
) -> Result<(), IdbError> {
    let (lower, upper) = range_bounds(range);
    // `key > upper` or `key >= upper` for every key: nothing, as SQLite's WHERE gave; redb, as a
    // BTreeMap, refuses a range that ends before it starts.
    let (lower_key, lower_open) = match &lower {
        Some((key, open)) => (key.as_slice(), *open),
        None => (&[][..], false),
    };
    if let Some((upper_key, upper_open)) = &upper &&
        (lower_key > upper_key.as_slice() ||
            (lower_key == upper_key.as_slice() && (lower_open || *upper_open)))
    {
        return Ok(());
    }
    let start = if lower_open {
        Bound::Excluded((store_id, lower_key))
    } else {
        Bound::Included((store_id, lower_key))
    };
    let end = match &upper {
        Some((key, true)) => Bound::Excluded((store_id, key.as_slice())),
        Some((key, false)) => Bound::Included((store_id, key.as_slice())),
        None => match store_id.checked_add(1) {
            Some(next) => Bound::Excluded((next, &[][..])),
            None => Bound::Unbounded,
        },
    };
    for row in data.range((start, end))? {
        let (key, value) = row?;
        let (row_store, key) = key.value();
        if row_store != store_id {
            break;
        }
        if !each(key, value.value()) {
            break;
        }
    }
    Ok(())
}

/// The rows of a range, at most `count` of them (Servo's `LIMIT`).
fn rows_in_range(
    data: &impl ReadableTable<(u64, &'static [u8]), &'static [u8]>,
    store_id: u64,
    range: &IndexedDBKeyRange,
    count: Option<u32>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>, IdbError> {
    let limit = count.map(|count| count as usize);
    let mut rows = vec![];
    if limit == Some(0) {
        return Ok(rows);
    }
    for_each_in_range(data, store_id, range, |key, value| {
        rows.push((key.to_vec(), value.to_vec()));
        limit.is_none_or(|limit| rows.len() < limit)
    })?;
    Ok(rows)
}

fn decode_key(key: &[u8]) -> Result<IndexedDBKeyType, IdbError> {
    encoding::deserialize(key).ok_or_else(|| corrupt("a key that does not decode"))
}

fn db_err(error: IdbError) -> BackendError {
    BackendError::DbErr(format!("{error:?}"))
}

pub struct RedbEngine {
    store: Arc<Store>,
    read_pool: Arc<ThreadPool>,
    write_pool: Arc<ThreadPool>,
    created_db_path: bool,
}

impl RedbEngine {
    pub fn new(
        path: PathBuf,
        created: bool,
        db_info: &IndexedDBDescription,
        pool: Arc<ThreadPool>,
    ) -> Result<Self, IdbError> {
        let db_path = path.join("indexeddb.redb");
        let store = open_shared(&db_path, |store| Self::init_db(store, db_info))?;
        Ok(Self {
            store,
            read_pool: pool.clone(),
            write_pool: pool,
            created_db_path: created,
        })
    }

    /// Returns whether the physical db was created as part of `new`.
    pub(crate) fn created_db_path(&self) -> bool {
        self.created_db_path
    }

    fn init_db(store: &Store, db_info: &IndexedDBDescription) -> Result<(), StoreError> {
        store.write(|txn| {
            if txn.list_tables()?.any(|table| table.name() == "database") {
                // Database already exists, no need to initialize
                return Ok(());
            }
            info!("Initializing indexeddb database at {:?}", store.path());
            txn.open_table(OBJECT_STORE)?;
            txn.open_table(OBJECT_STORE_BY_NAME)?;
            txn.open_table(OBJECT_DATA)?;
            txn.open_table(OBJECT_STORE_INDEX)?;
            txn.open_table(SEQUENCES)?;
            // From https://w3c.github.io/IndexedDB/#database-version:
            // "When a database is first created, its version is 0 (zero)."
            let record = database_record(&db_info.origin.ascii_serialization(), 0)?;
            txn.open_table(DATABASE)?
                .insert(db_info.name.as_bytes(), record.as_slice())?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(crate) fn object_store_by_name(
        store: &Store,
        store_name: &str,
    ) -> Result<ObjectStore, IdbError> {
        find_store_in_read(store, store_name)?.ok_or(IdbError::NoRow("object_store"))
    }

    fn get(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>, IdbError> {
        let txn = store.read()?;
        let data = txn.open_table(OBJECT_DATA)?;
        Ok(rows_in_range(&data, object_store.id, &key_range, Some(1))?
            .into_iter()
            .next())
    }

    fn get_key(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
    ) -> Result<Option<Vec<u8>>, IdbError> {
        Self::get(store, object_store, key_range).map(|row| row.map(|(key, _)| key))
    }

    fn get_item(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
    ) -> Result<Option<Vec<u8>>, IdbError> {
        Self::get(store, object_store, key_range).map(|row| row.map(|(_, data)| data))
    }

    fn get_all(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
        count: Option<u32>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, IdbError> {
        let txn = store.read()?;
        let data = txn.open_table(OBJECT_DATA)?;
        rows_in_range(&data, object_store.id, &key_range, count)
    }

    pub(crate) fn get_all_keys(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
        count: Option<u32>,
    ) -> Result<Vec<Vec<u8>>, IdbError> {
        Self::get_all(store, object_store, key_range, count)
            .map(|rows| rows.into_iter().map(|(key, _)| key).collect())
    }

    fn get_all_items(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
        count: Option<u32>,
    ) -> Result<Vec<Vec<u8>>, IdbError> {
        Self::get_all(store, object_store, key_range, count)
            .map(|rows| rows.into_iter().map(|(_, data)| data).collect())
    }

    pub(crate) fn put_item(
        store: &Store,
        object_store: &ObjectStore,
        key: IndexedDBKeyType,
        value: Vec<u8>,
        should_overwrite: bool,
        key_generator_current_number: Option<i64>,
    ) -> Result<PutItemResult, IdbError> {
        let no_overwrite = !should_overwrite;
        let serialized_key: Vec<u8> = encoding::serialize(&key);
        write_or_roll_back(store, Commit::Soon, |txn| {
            {
                let mut data = txn.open_table(OBJECT_DATA)?;
                let existing_item = data
                    .get((object_store.id, serialized_key.as_slice()))?
                    .is_some();
                if existing_item && no_overwrite {
                    return Ok(PutItemResult::CannotOverwrite);
                }
                // Preserve `put()` semantics by replacing the stored value when the primary
                // key already exists.
                data.insert(
                    (object_store.id, serialized_key.as_slice()),
                    value.as_slice(),
                )?;
            }
            if let Some(next_key_generator_current_number) = key_generator_current_number {
                // UPDATE object_store SET auto_increment = ? WHERE id = ? (no row: no change)
                if let Some(mut row) = find_store_in_write(txn, &object_store.name)?
                    .filter(|row| row.id == object_store.id)
                {
                    row.auto_increment = next_key_generator_current_number;
                    write_store(txn, &row)?;
                }
            }
            Ok(PutItemResult::Key(key))
        })?
    }

    pub(crate) fn delete_item(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
    ) -> Result<(), IdbError> {
        write_or_roll_back(store, Commit::Soon, |txn| {
            let mut data = txn.open_table(OBJECT_DATA)?;
            let mut keys = vec![];
            for_each_in_range(&data, object_store.id, &key_range, |key, _| {
                keys.push(key.to_vec());
                true
            })?;
            for key in keys {
                data.remove((object_store.id, key.as_slice()))?;
            }
            Ok(())
        })?
    }

    fn clear(store: &Store, object_store: &ObjectStore) -> Result<(), IdbError> {
        Self::delete_item(store, object_store, IndexedDBKeyRange::default())
    }

    fn count(
        store: &Store,
        object_store: &ObjectStore,
        key_range: IndexedDBKeyRange,
    ) -> Result<usize, IdbError> {
        let txn = store.read()?;
        let data = txn.open_table(OBJECT_DATA)?;
        let mut count = 0;
        for_each_in_range(&data, object_store.id, &key_range, |_, _| {
            count += 1;
            true
        })?;
        Ok(count)
    }

    fn create_store(
        store: &Store,
        store_name: &str,
        key_path: Option<KeyPath>,
        auto_increment: bool,
        commit: Commit,
    ) -> Result<CreateObjectResult, IdbError> {
        let key_path = key_path
            .map(|key_path| postcard::to_stdvec(&key_path))
            .transpose()
            .map_err(|error| IdbError::Serialize(error.to_string()))?;
        write_or_roll_back(store, commit, |txn| {
            if find_store_in_write(txn, store_name)?.is_some() {
                // Store already exists
                return Ok(CreateObjectResult::AlreadyExists);
            }
            let row = ObjectStore {
                id: next_id(txn, "object_store")?,
                name: store_name.to_owned(),
                key_path,
                auto_increment: auto_increment as i64,
            };
            write_store(txn, &row)?;
            txn.open_table(OBJECT_STORE_BY_NAME)?
                .insert(name_key(store_name).as_slice(), row.id)?;
            Ok(CreateObjectResult::Created)
        })?
    }

    fn delete_store(store: &Store, store_name: &str, commit: Commit) -> Result<(), IdbError> {
        // https://www.w3.org/TR/IndexedDB-3/#dom-idbdatabase-deleteobjectstore
        // Step 7. Destroy store.
        write_or_roll_back(store, commit, |txn| {
            let object_store =
                find_store_in_write(txn, store_name)?.ok_or(IdbError::NoRow("object_store"))?;
            let id = object_store.id;

            // DELETE FROM object_store_index WHERE object_store_id = ?
            {
                let mut indexes = txn.open_table(OBJECT_STORE_INDEX)?;
                let mut names = vec![];
                for row in indexes.range((id, &[][..])..)? {
                    let (key, _) = row?;
                    let (row_store, name) = key.value();
                    if row_store != id {
                        break;
                    }
                    names.push(name.to_vec());
                }
                for name in names {
                    indexes.remove((id, name.as_slice()))?;
                }
            }
            // DELETE FROM object_data WHERE object_store_id = ?
            {
                let mut data = txn.open_table(OBJECT_DATA)?;
                let mut keys = vec![];
                for_each_in_range(&data, id, &IndexedDBKeyRange::default(), |key, _| {
                    keys.push(key.to_vec());
                    true
                })?;
                for key in keys {
                    data.remove((id, key.as_slice()))?;
                }
            }
            // DELETE FROM object_store WHERE id = ?
            txn.open_table(OBJECT_STORE)?.remove(id)?;
            txn.open_table(OBJECT_STORE_BY_NAME)?
                .remove(name_key(store_name).as_slice())?;
            Ok(())
        })?
    }

    fn create_index(
        store: &Store,
        store_name: &str,
        index_name: String,
        key_path: KeyPath,
        unique: bool,
        multi_entry: bool,
        commit: Commit,
    ) -> Result<CreateObjectResult, IdbError> {
        let key_path = postcard::to_stdvec(&key_path)
            .map_err(|error| IdbError::Serialize(error.to_string()))?;
        write_or_roll_back(store, commit, |txn| {
            let object_store =
                find_store_in_write(txn, store_name)?.ok_or(IdbError::NoRow("object_store"))?;
            let key = name_key(&index_name);
            let index_exists = txn
                .open_table(OBJECT_STORE_INDEX)?
                .get((object_store.id, key.as_slice()))?
                .is_some();
            if index_exists {
                return Ok(CreateObjectResult::AlreadyExists);
            }
            let index = Index {
                id: next_id(txn, "object_store_index")?,
                name: index_name,
                key_path,
                unique,
                multi_entry,
            };
            txn.open_table(OBJECT_STORE_INDEX)?.insert(
                (object_store.id, key.as_slice()),
                index.record()?.as_slice(),
            )?;
            Ok(CreateObjectResult::Created)
        })?
    }

    fn rename_index(
        store: &Store,
        store_name: &str,
        index_name: &str,
        new_name: &str,
        commit: Commit,
    ) -> Result<(), IdbError> {
        write_or_roll_back(store, commit, |txn| {
            let object_store =
                find_store_in_write(txn, store_name)?.ok_or(IdbError::NoRow("object_store"))?;
            // Rename the index if it exists
            // (UPDATE object_store_index SET name = ? WHERE name = ? AND object_store_id = ?)
            let old_key = name_key(index_name);
            let new_key = name_key(new_name);
            let mut indexes = txn.open_table(OBJECT_STORE_INDEX)?;
            let Some(record) = indexes
                .get((object_store.id, old_key.as_slice()))?
                .map(|record| record.value().to_vec())
            else {
                return Ok(());
            };
            if old_key == new_key {
                return Ok(());
            }
            if indexes
                .get((object_store.id, new_key.as_slice()))?
                .is_some()
            {
                return Err(IdbError::Unique(
                    "object_store_index.object_store_id, object_store_index.name",
                ));
            }
            let mut index = Index::read(&record)?;
            index.name = new_name.to_owned();
            indexes.remove((object_store.id, old_key.as_slice()))?;
            indexes.insert(
                (object_store.id, new_key.as_slice()),
                index.record()?.as_slice(),
            )?;
            Ok(())
        })?
    }

    fn delete_index(
        store: &Store,
        store_name: &str,
        index_name: String,
        commit: Commit,
    ) -> Result<(), IdbError> {
        write_or_roll_back(store, commit, |txn| {
            let object_store =
                find_store_in_write(txn, store_name)?.ok_or(IdbError::NoRow("object_store"))?;
            // Delete the index if it exists
            txn.open_table(OBJECT_STORE_INDEX)?
                .remove((object_store.id, name_key(&index_name).as_slice()))?;
            Ok(())
        })?
    }

    /// One request of a transaction, as Servo ran it: its own statement (here its own read or
    /// write transaction), so that a request that fails does not undo or stop the others.
    ///
    /// LucinaNet: the requests of a readwrite transaction are not grouped into one write
    /// transaction. One write transaction would hold results that a failed commit at the end
    /// could take back, and a request failing halfway would leave its first writes in it where
    /// SQLite rolled back the failed statement. One `store.write` per request keeps every result
    /// as it was.
    ///
    /// The result is returned as a [`Reply`], sent once the transaction's writes are durable
    /// (see [`run_transaction`]).
    fn process_request(store: &Store, store_name: &str, operation: AsyncOperation) -> Reply {
        if let AsyncOperation::Schema(AsyncSchemaOperation::CreateObjectStore {
            callback,
            key_path,
            auto_increment,
        }) = &operation
        {
            let result = Self::create_store(
                store,
                store_name,
                key_path.clone(),
                *auto_increment,
                Commit::Soon,
            )
            .map(|_| ())
            .map_err(db_err);
            return Reply::schema(callback.clone(), result);
        }

        let object_store = match find_store_in_read(store, store_name) {
            Ok(Some(object_store)) => object_store,
            Ok(None) => return Reply::failed(operation, BackendError::StoreNotFound),
            Err(error) => {
                return Reply::failed(operation, BackendError::DbErr(format!("{error:?}")));
            },
        };

        match operation {
            AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                callback,
                key,
                value,
                should_overwrite,
                key_generator_current_number,
            }) => {
                let (key, key_generator_current_number) = match key {
                    Some(key) => (key, key_generator_current_number),
                    None => {
                        if object_store.auto_increment == 0 {
                            return Reply::now(move || {
                                if let Err(error) = callback.send(Err(BackendError::DbErr(
                                    "Missing key for PutItem request".to_string(),
                                ))) {
                                    warn!("Failed to send PutItem missing key error: {error:?}");
                                }
                            });
                        }
                        let Some(next_key_generator_current_number) =
                            object_store.auto_increment.checked_add(1)
                        else {
                            return Reply::now(move || {
                                if let Err(error) = callback.send(Err(BackendError::DbErr(
                                    "Key generator overflow".to_string(),
                                ))) {
                                    warn!(
                                        "Failed to send PutItem key generator overflow error: {error:?}"
                                    );
                                }
                            });
                        };
                        (
                            IndexedDBKeyType::Number(object_store.auto_increment as f64),
                            Some(next_key_generator_current_number),
                        )
                    },
                };
                let result = Self::put_item(
                    store,
                    &object_store,
                    key,
                    value,
                    should_overwrite,
                    key_generator_current_number,
                )
                .map_err(db_err);
                Reply::write(callback, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetItem {
                callback,
                key_range,
            }) => {
                let result = Self::get_item(store, &object_store, key_range).map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetAllKeys {
                callback,
                key_range,
                count,
            }) => {
                let result = Self::get_all_keys(store, &object_store, key_range, count)
                    .and_then(|keys| keys.iter().map(|key| decode_key(key)).collect())
                    .map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetAllItems {
                callback,
                key_range,
                count,
            }) => {
                let result =
                    Self::get_all_items(store, &object_store, key_range, count).map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::ReadWrite(AsyncReadWriteOperation::RemoveItem {
                callback,
                key_range,
            }) => {
                let result = Self::delete_item(store, &object_store, key_range).map_err(db_err);
                Reply::write(callback, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::Count {
                callback,
                key_range,
            }) => {
                let result = Self::count(store, &object_store, key_range)
                    .map(|count| count as u64)
                    .map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::Iterate {
                callback,
                key_range,
            }) => {
                let result = Self::get_all(store, &object_store, key_range, None)
                    .and_then(|records| {
                        records
                            .into_iter()
                            .map(|(key, data)| {
                                Ok(IndexedDBRecord {
                                    key: decode_key(&key)?,
                                    primary_key: decode_key(&key)?,
                                    value: data,
                                })
                            })
                            .collect()
                    })
                    .map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::ReadWrite(AsyncReadWriteOperation::Clear(sender)) => {
                let result = Self::clear(store, &object_store).map_err(db_err);
                Reply::write(sender, result)
            },
            AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetKey {
                callback,
                key_range,
            }) => {
                let result = Self::get_key(store, &object_store, key_range)
                    .and_then(|key| key.map(|key| decode_key(&key)).transpose())
                    .map_err(db_err);
                Reply::read(callback, result)
            },
            AsyncOperation::Schema(AsyncSchemaOperation::CreateIndex {
                callback,
                index_name,
                key_path,
                unique,
                multi_entry,
            }) => {
                let result = Self::create_index(
                    store,
                    store_name,
                    index_name,
                    key_path,
                    unique,
                    multi_entry,
                    Commit::Soon,
                )
                .map(|_| ())
                .map_err(db_err);
                Reply::schema(callback, result)
            },
            AsyncOperation::Schema(AsyncSchemaOperation::CreateObjectStore { .. }) => {
                // Handled above, before the store is looked up.
                Reply::now(|| {})
            },
            AsyncOperation::Schema(AsyncSchemaOperation::DeleteIndex {
                index_name,
                callback,
            }) => {
                let result =
                    Self::delete_index(store, store_name, index_name, Commit::Soon).map_err(db_err);
                Reply::schema(callback, result)
            },
            AsyncOperation::Schema(AsyncSchemaOperation::DeleteObjectStore { callback }) => {
                let result = Self::delete_store(store, store_name, Commit::Soon).map_err(db_err);
                Reply::schema(callback, result)
            },
            AsyncOperation::Schema(AsyncSchemaOperation::RenameIndex {
                index_name,
                new_name,
                callback,
            }) => {
                let result =
                    Self::rename_index(store, store_name, &index_name, &new_name, Commit::Soon)
                        .map_err(db_err);
                Reply::schema(callback, result)
            },
        }
    }
}

/// The answer to one request, held until the transaction's writes are durable.
pub(crate) struct Reply(Box<dyn FnOnce(Option<&BackendError>) + Send>);

impl Reply {
    /// An answer that does not depend on the transaction's writes being kept.
    fn now(send: impl FnOnce() + Send + 'static) -> Reply {
        Reply(Box::new(move |_| send()))
    }

    fn failed(operation: AsyncOperation, error: BackendError) -> Reply {
        Reply::now(move || operation.notify_error(error))
    }

    fn read<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Send + 'static>(
        callback: GenericCallback<BackendResult<T>>,
        result: BackendResult<T>,
    ) -> Reply {
        Reply::now(move || {
            let _ = callback.send(result);
        })
    }

    /// The result of a write: if the transaction could not be made durable, the write is not
    /// kept, and the request fails with that error.
    fn write<T: serde::Serialize + for<'de> serde::Deserialize<'de> + Send + 'static>(
        callback: GenericCallback<BackendResult<T>>,
        result: BackendResult<T>,
    ) -> Reply {
        Reply(Box::new(move |not_durable| {
            let result = match (result, not_durable) {
                (Ok(_), Some(error)) => Err(error.clone()),
                (result, _) => result,
            };
            let _ = callback.send(result);
        }))
    }

    /// A schema change, whose callback hears only of errors.
    fn schema(callback: GenericCallback<BackendError>, result: Result<(), BackendError>) -> Reply {
        Reply(Box::new(move |not_durable| {
            let error = match result {
                Err(error) => Some(error),
                Ok(()) => not_durable.cloned(),
            };
            if let Some(error) = error {
                let _ = callback.send(error);
            }
        }))
    }
}

/// Runs the requests of one transaction, makes its writes durable with `make_durable` unless it
/// is readonly, sends every request's answer, in order, then calls `on_complete`.
///
/// LucinaNet: a commit of the profile store becomes durable within its flush delay, and is lost
/// if the process dies before that. Servo's SQLite (WAL, `synchronous = NORMAL`) kept a
/// committed transaction through a crash of the process, so script, once it hears `complete`,
/// may rely on the data being there: here a transaction that may have written is made durable
/// before any of its answers is sent and before `on_complete`. If that fails, the requests that
/// wrote fail with the error (the same path as a failed request), the others keep their answers,
/// and `on_complete` is still called, as Servo did.
pub(crate) fn run_transaction(
    store: &Store,
    transaction: KvsTransaction,
    make_durable: impl FnOnce(&Store) -> Result<(), StoreError>,
    on_complete: Box<dyn FnOnce() + Send + 'static>,
) {
    let readonly = transaction.mode == IndexedDBTxnMode::Readonly;
    let replies: Vec<Reply> = transaction
        .requests
        .into_iter()
        .map(|request| RedbEngine::process_request(store, &request.store_name, request.operation))
        .collect();
    let not_durable = if readonly {
        None
    } else {
        make_durable(store)
            .err()
            .map(|error| BackendError::DbErr(format!("{:?}", IdbError::from(error))))
    };
    for reply in replies {
        (reply.0)(not_durable.as_ref());
    }
    on_complete();
}

/// Makes every commit so far durable: one durable commit, which waits for any write in progress
/// (the profile store's flush thread included) and makes every earlier commit durable with it.
/// `Store::flush` returns at once when that thread has just taken the pending commits, before
/// they are on disk.
pub(crate) fn make_durable(store: &Store) -> Result<(), StoreError> {
    store.write_durable(|_| Ok(()))
}

// LucinaNet: what this engine changes outside `process_transaction` (the manager's version
// changes, upgrade aborts and the restoring of their snapshots) is committed durably, for the
// reason given at `run_transaction`.
impl KvsEngine for RedbEngine {
    type Error = IdbError;

    fn create_store(
        &self,
        store_name: &str,
        key_path: Option<KeyPath>,
        auto_increment: bool,
    ) -> Result<CreateObjectResult, Self::Error> {
        Self::create_store(
            &self.store,
            store_name,
            key_path,
            auto_increment,
            Commit::Durable,
        )
    }

    fn create_index(
        &self,
        store_name: &str,
        index_name: String,
        key_path: KeyPath,
        unique: bool,
        multi_entry: bool,
    ) -> Result<CreateObjectResult, Self::Error> {
        Self::create_index(
            &self.store,
            store_name,
            index_name,
            key_path,
            unique,
            multi_entry,
            Commit::Durable,
        )
    }

    fn delete_store(&self, store_name: &str) -> Result<(), Self::Error> {
        Self::delete_store(&self.store, store_name, Commit::Durable)
    }

    fn close_store(&self, _store_name: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn process_transaction(
        &self,
        transaction: KvsTransaction,
        on_complete: Box<dyn FnOnce() + Send + 'static>,
    ) {
        let spawning_pool = if transaction.mode == IndexedDBTxnMode::Readonly {
            self.read_pool.clone()
        } else {
            self.write_pool.clone()
        };
        // Servo opened a new SQLite connection to the file here, on the pool; a redb file is open
        // once per process, so the pool shares this engine's store.
        let store = self.store.clone();
        spawning_pool.spawn(move || {
            run_transaction(&store, transaction, make_durable, on_complete);
        });
    }

    fn key_generator_current_number(&self, store_name: &str) -> Option<i64> {
        // Servo panicked on an error of the database here; this reports it and returns None.
        match find_store_in_read(&self.store, store_name) {
            Ok(object_store) => object_store
                .map(|object_store| object_store.auto_increment)
                .and_then(|current_number| (current_number != 0).then_some(current_number)),
            Err(error) => {
                warn!("IndexedDB: key generator of {store_name:?}: {error}");
                None
            },
        }
    }

    fn set_key_generator_current_number(
        &self,
        store_name: &str,
        current_number: i64,
    ) -> Result<(), Self::Error> {
        write_or_roll_back(&self.store, Commit::Durable, |txn| {
            // A missing store is reported as QueryReturnedNoRows.
            let mut object_store =
                find_store_in_write(txn, store_name)?.ok_or(IdbError::NoRow("object_store"))?;
            object_store.auto_increment = current_number;
            write_store(txn, &object_store)
        })?
    }

    fn key_path(&self, store_name: &str) -> Option<KeyPath> {
        // Servo panicked on an error of the database or a key path that did not decode; this
        // reports it and returns None, as for a store without a key path.
        let key_path = match find_store_in_read(&self.store, store_name) {
            Ok(object_store) => object_store?.key_path?,
            Err(error) => {
                warn!("IndexedDB: key path of {store_name:?}: {error}");
                return None;
            },
        };
        match postcard::from_bytes(&key_path) {
            Ok(key_path) => Some(key_path),
            Err(error) => {
                warn!("IndexedDB: key path of {store_name:?} does not decode: {error}");
                None
            },
        }
    }

    fn object_store_names(&self) -> Result<Vec<String>, Self::Error> {
        let txn = self.store.read()?;
        let names = txn.open_table(OBJECT_STORE_BY_NAME)?;
        let stores = txn.open_table(OBJECT_STORE)?;
        let mut result = vec![];
        for row in names.iter()? {
            let (_, id) = row?;
            let id = id.value();
            let record = stores
                .get(id)?
                .ok_or_else(|| corrupt(format!("object store {id} without its row")))?;
            result.push(ObjectStore::read(id, record.value())?.name);
        }
        Ok(result)
    }

    fn indexes(&self, store_name: &str) -> Result<Vec<IndexedDBIndex>, Self::Error> {
        let txn = self.store.read()?;
        let object_store = find_store(
            &txn.open_table(OBJECT_STORE_BY_NAME)?,
            &txn.open_table(OBJECT_STORE)?,
            store_name,
        )?
        .ok_or(IdbError::NoRow("object_store"))?;
        let indexes = txn.open_table(OBJECT_STORE_INDEX)?;
        let mut result = vec![];
        for row in indexes.range((object_store.id, &[][..])..)? {
            let (key, record) = row?;
            if key.value().0 != object_store.id {
                break;
            }
            let index = Index::read(record.value())?;
            let key_path = postcard::from_bytes(&index.key_path)
                .map_err(|error| corrupt(format!("key path of index {} ({error})", index.id)))?;
            result.push(IndexedDBIndex {
                name: index.name,
                key_path,
                unique: index.unique,
                multi_entry: index.multi_entry,
            });
        }
        Ok(result)
    }

    fn delete_index(&self, store_name: &str, index_name: String) -> Result<(), Self::Error> {
        Self::delete_index(&self.store, store_name, index_name, Commit::Durable)
    }

    fn version(&self) -> Result<u64, Self::Error> {
        // SELECT version FROM database LIMIT 1
        let txn = self.store.read()?;
        let database = txn.open_table(DATABASE)?;
        let (_, record) = database.first()?.ok_or(IdbError::NoRow("database"))?;
        Ok(read_database(record.value())?.1)
    }

    fn set_version(&self, version: u64) -> Result<(), Self::Error> {
        // UPDATE database SET version = ?
        write_or_roll_back(&self.store, Commit::Durable, |txn| {
            let mut database = txn.open_table(DATABASE)?;
            let mut rows = vec![];
            for row in database.iter()? {
                let (name, record) = row?;
                let (origin, _) = read_database(record.value())?;
                rows.push((name.value().to_vec(), origin));
            }
            if rows.is_empty() {
                return Err(IdbError::NoRow("database"));
            }
            for (name, origin) in rows {
                database.insert(
                    name.as_slice(),
                    database_record(&origin, version)?.as_slice(),
                )?;
            }
            Ok(())
        })?
    }
}

impl MallocSizeOf for RedbEngine {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        // Servo added what SQLite reported of its caches through its C API. redb's page cache
        // tells nothing of its size, so only what this struct owns is counted.
        self.created_db_path.size_of(ops)
    }
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Arc;

    use profile_store::redb::ReadableTable;
    use profile_traits::generic_callback::GenericCallback;
    use serde::{Deserialize, Serialize};
    use servo_base::generic_channel::{self, GenericReceiver, GenericSender};
    use servo_base::id::{PIPELINE_NAMESPACE, PipelineNamespace, PipelineNamespaceId, WebViewId};
    use servo_base::threadpool::ThreadPool;
    use servo_url::ImmutableOrigin;
    use storage_traits::client_storage::{
        ClientStorageThreadHandle, StorageIdentifier, StorageProxyMap, StorageType,
    };
    use storage_traits::indexeddb::{
        AsyncOperation, AsyncReadOnlyOperation, AsyncReadWriteOperation, AsyncSchemaOperation,
        BackendError, CreateObjectResult, IndexedDBKeyRange, IndexedDBKeyType, IndexedDBTxnMode,
        KeyPath, PutItemResult,
    };
    use url::Host;

    use super::{DATABASE, OBJECT_DATA, OBJECT_STORE, OBJECT_STORE_INDEX, name_key};
    use crate::ClientStorageThreadFactory;
    use crate::indexeddb::IndexedDBDescription;
    use crate::indexeddb::engines::{
        KvsEngine, KvsOperation, KvsTransaction, RedbEngine, encoding,
    };
    use crate::shared::Commit;

    fn install_test_namespace() {
        PipelineNamespace::install(PipelineNamespaceId(1));
    }

    fn test_origin() -> ImmutableOrigin {
        ImmutableOrigin::Tuple(
            "test_origin".to_string(),
            Host::Domain("localhost".to_string()),
            80,
        )
    }

    fn get_pool() -> Arc<ThreadPool> {
        ThreadPool::global()
    }

    /// `SELECT COUNT(*) FROM object_data WHERE object_store_id = ?`, read from the store.
    fn object_data_rows(db: &RedbEngine, store_id: u64) -> i64 {
        let txn = db.store.read().unwrap();
        let data = txn.open_table(OBJECT_DATA).unwrap();
        let mut count = 0;
        for row in data.iter().unwrap() {
            let (key, _) = row.unwrap();
            if key.value().0 == store_id {
                count += 1;
            }
        }
        count
    }

    fn open_engine(path: PathBuf, created: bool) -> RedbEngine {
        RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            get_pool(),
        )
        .unwrap()
    }

    fn create_db(
        db_name: String,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        bool,
        StorageProxyMap,
        ClientStorageThreadHandle,
    ) {
        if PIPELINE_NAMESPACE.get().is_none() {
            install_test_namespace();
        }
        let tmp_dir = tempfile::tempdir().unwrap();
        let handle: ClientStorageThreadHandle =
            ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), true);

        let storage_proxy_map = handle
            .obtain_a_storage_bottle_map(
                StorageType::Local,
                Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID)),
                StorageIdentifier::IndexedDB,
                test_origin(),
            )
            .recv()
            .unwrap()
            .unwrap();
        let (path, created) = handle
            .create_database(storage_proxy_map.bottle_id, db_name)
            .recv()
            .unwrap()
            .unwrap();
        (tmp_dir, path, created, storage_proxy_map, handle)
    }

    #[test]
    fn test_cycle() {
        let (_temp_dir, path, created, proxy_map, handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        // Test create
        let db = RedbEngine::new(
            path.clone(),
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool.clone(),
        )
        .unwrap();
        drop(db);

        // Test open
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool.clone(),
        )
        .unwrap();
        let version = db.version().expect("Failed to get version");
        assert_eq!(version, 0);
        db.set_version(5).unwrap();
        let new_version = db.version().expect("Failed to get new version");
        assert_eq!(new_version, 5);
        drop(db);
        handle
            .delete_database(proxy_map.bottle_id, "test_db".to_string())
            .recv()
            .unwrap()
            .expect("Failed to delete database");
    }

    #[test]
    fn test_create_store() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        let store_name = "test_store";
        let result = db.create_store(store_name, None, true);
        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result, CreateObjectResult::Created);
        // Try to create the same store again
        let result = db.create_store(store_name, None, false);
        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result, CreateObjectResult::AlreadyExists);
        // Ensure store was not overwritten
        assert!(db.key_generator_current_number(store_name).is_some());
    }

    #[test]
    fn test_create_store_empty_name() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        let store_name = "";
        let result = db.create_store(store_name, None, true);
        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result, CreateObjectResult::Created);
    }

    #[test]
    fn test_injection() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        // Create a normal store
        let store_name1 = "test_store";
        let result = db.create_store(store_name1, None, true);
        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result, CreateObjectResult::Created);
        // Injection
        let store_name2 = "' OR 1=1 -- -";
        let result = db.create_store(store_name2, None, false);
        assert!(result.is_ok());
        let create_result = result.unwrap();
        assert_eq!(create_result, CreateObjectResult::Created);
    }

    #[test]
    fn test_key_path() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        let store_name = "test_store";
        let result = db.create_store(store_name, Some(KeyPath::String("test".to_string())), true);
        assert!(result.is_ok());
        assert_eq!(
            db.key_path(store_name),
            Some(KeyPath::String("test".to_string()))
        );
    }

    #[test]
    fn test_delete_store() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        db.create_store("test_store", None, false)
            .expect("Failed to create store");
        // Delete the store
        db.delete_store("test_store")
            .expect("Failed to delete store");
        // Try to delete the same store again
        let result = db.delete_store("test_store");
        assert!(result.is_err());
        // Try to delete a non-existing store
        let result = db.delete_store("test_store");
        // Should work as per spec
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_store_removes_store_records() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();

        db.create_store("test_store", None, false)
            .expect("Failed to create store");
        let object_store = RedbEngine::object_store_by_name(&db.store, "test_store")
            .expect("Failed to fetch store metadata");
        RedbEngine::put_item(
            &db.store,
            &object_store,
            IndexedDBKeyType::Number(1.0),
            vec![1, 2, 3],
            true,
            None,
        )
        .expect("Failed to insert item");

        let row_count_before = object_data_rows(&db, object_store.id);
        assert_eq!(row_count_before, 1);

        db.delete_store("test_store")
            .expect("Failed to delete store");

        let row_count_after = object_data_rows(&db, object_store.id);
        assert_eq!(row_count_after, 0);
    }

    #[test]
    fn test_async_operations() {
        fn get_channel<T>() -> (GenericSender<T>, GenericReceiver<T>)
        where
            T: for<'de> Deserialize<'de> + Serialize,
        {
            generic_channel::channel().unwrap()
        }

        fn get_callback<T>(chan: GenericSender<T>) -> GenericCallback<T>
        where
            T: for<'de> Deserialize<'de> + Serialize + Send + Sync,
        {
            GenericCallback::new(move |r| {
                assert!(chan.send(r.unwrap()).is_ok());
            })
            .expect("Could not construct callback")
        }

        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let thread_pool = get_pool();
        let db = RedbEngine::new(
            path,
            created,
            &IndexedDBDescription {
                name: "test_db".to_string(),
                origin: test_origin(),
            },
            thread_pool,
        )
        .unwrap();
        let store_name = "test_store";
        db.create_store(store_name, None, false)
            .expect("Failed to create store");
        let put = get_channel();
        let put2 = get_channel();
        let put3 = get_channel();
        let put_dup = get_channel();
        let put_overwrite = get_channel();
        let get_item_some = get_channel();
        let get_item_none = get_channel();
        let get_all_items = get_channel();
        let count = get_channel();
        let remove = get_channel();
        let clear = get_channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        db.process_transaction(
            KvsTransaction {
                mode: IndexedDBTxnMode::Readwrite,
                requests: VecDeque::from(vec![
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                            callback: get_callback(put.0),
                            key: Some(IndexedDBKeyType::Number(1.0)),
                            value: vec![1, 2, 3],
                            should_overwrite: false,
                            key_generator_current_number: None,
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                            callback: get_callback(put2.0),
                            key: Some(IndexedDBKeyType::String("2.0".to_string())),
                            value: vec![4, 5, 6],
                            should_overwrite: false,
                            key_generator_current_number: None,
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                            callback: get_callback(put3.0),
                            key: Some(IndexedDBKeyType::Array(vec![
                                IndexedDBKeyType::String("3".to_string()),
                                IndexedDBKeyType::Number(0.0),
                            ])),
                            value: vec![7, 8, 9],
                            should_overwrite: false,
                            key_generator_current_number: None,
                        }),
                    },
                    // Try to put a duplicate key without overwrite
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                            callback: get_callback(put_dup.0),
                            key: Some(IndexedDBKeyType::Number(1.0)),
                            value: vec![10, 11, 12],
                            should_overwrite: false,
                            key_generator_current_number: None,
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                            callback: get_callback(put_overwrite.0),
                            key: Some(IndexedDBKeyType::Number(1.0)),
                            value: vec![13, 14, 15],
                            should_overwrite: true,
                            key_generator_current_number: None,
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetItem {
                            callback: get_callback(get_item_some.0),
                            key_range: IndexedDBKeyRange::only(IndexedDBKeyType::Number(1.0)),
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetItem {
                            callback: get_callback(get_item_none.0),
                            key_range: IndexedDBKeyRange::only(IndexedDBKeyType::Number(5.0)),
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetAllItems {
                            callback: get_callback(get_all_items.0),
                            key_range: IndexedDBKeyRange::lower_bound(
                                IndexedDBKeyType::Number(0.0),
                                false,
                            ),
                            count: None,
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::Count {
                            callback: get_callback(count.0),
                            key_range: IndexedDBKeyRange::only(IndexedDBKeyType::Number(1.0)),
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::RemoveItem {
                            callback: get_callback(remove.0),
                            key_range: IndexedDBKeyRange::only(IndexedDBKeyType::Number(1.0)),
                        }),
                    },
                    KvsOperation {
                        store_name: store_name.to_owned(),
                        operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::Clear(
                            get_callback(clear.0),
                        )),
                    },
                ]),
            },
            Box::new(move || {
                let _ = done_tx.send(());
            }),
        );
        let _ = done_rx.recv().unwrap();
        put.1.recv().unwrap().unwrap();
        put2.1.recv().unwrap().unwrap();
        put3.1.recv().unwrap().unwrap();
        let err = put_dup.1.recv().unwrap().unwrap();
        assert_eq!(err, PutItemResult::CannotOverwrite);
        let overwritten = put_overwrite.1.recv().unwrap().unwrap();
        assert_eq!(
            overwritten,
            PutItemResult::Key(IndexedDBKeyType::Number(1.0))
        );
        let get_result = get_item_some.1.recv().unwrap();
        let value = get_result.unwrap();
        assert_eq!(value, Some(vec![13, 14, 15]));
        let get_result = get_item_none.1.recv().unwrap();
        let value = get_result.unwrap();
        assert_eq!(value, None);
        let all_items = get_all_items.1.recv().unwrap().unwrap();
        assert_eq!(all_items.len(), 3);
        // Check that all three items are present
        assert!(all_items.contains(&vec![13, 14, 15]));
        assert!(all_items.contains(&vec![4, 5, 6]));
        assert!(all_items.contains(&vec![7, 8, 9]));
        let amount = count.1.recv().unwrap().unwrap();
        assert_eq!(amount, 1);
        remove.1.recv().unwrap().unwrap();
        clear.1.recv().unwrap().unwrap();
    }

    #[test]
    fn test_delete_item_range_respects_open_bounds() {
        fn remaining_keys_after_delete(
            lower: i32,
            upper: i32,
            lower_open: bool,
            upper_open: bool,
        ) -> Vec<i32> {
            let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
            let thread_pool = get_pool();
            let db = RedbEngine::new(
                path,
                created,
                &IndexedDBDescription {
                    name: "test_db".to_string(),
                    origin: test_origin(),
                },
                thread_pool,
            )
            .unwrap();
            let store_name = "test_store";
            db.create_store(store_name, None, false)
                .expect("Failed to create store");
            let store = RedbEngine::object_store_by_name(&db.store, store_name)
                .expect("Failed to get object store");

            for key in 1..=10 {
                RedbEngine::put_item(
                    &db.store,
                    &store,
                    IndexedDBKeyType::Number(key as f64),
                    vec![key as u8],
                    false,
                    None,
                )
                .expect("Failed to seed object store");
            }

            RedbEngine::delete_item(
                &db.store,
                &store,
                IndexedDBKeyRange::new(
                    Some(IndexedDBKeyType::Number(lower as f64)),
                    Some(IndexedDBKeyType::Number(upper as f64)),
                    lower_open,
                    upper_open,
                ),
            )
            .expect("Failed to delete key range");

            RedbEngine::get_all_keys(&db.store, &store, IndexedDBKeyRange::default(), None)
                .expect("Failed to read remaining keys")
                .into_iter()
                .map(|raw_key| match encoding::deserialize(&raw_key).unwrap() {
                    IndexedDBKeyType::Number(number) => number as i32,
                    other => panic!("Expected numeric key, got {other:?}"),
                })
                .collect()
        }

        assert_eq!(
            remaining_keys_after_delete(3, 8, false, false),
            vec![1, 2, 9, 10]
        );
        assert_eq!(
            remaining_keys_after_delete(3, 8, true, false),
            vec![1, 2, 3, 9, 10]
        );
        assert_eq!(
            remaining_keys_after_delete(3, 8, false, true),
            vec![1, 2, 8, 9, 10]
        );
        assert_eq!(
            remaining_keys_after_delete(3, 8, true, true),
            vec![1, 2, 3, 8, 9, 10]
        );
    }

    fn number_keys(
        db: &RedbEngine,
        store: &super::ObjectStore,
        range: IndexedDBKeyRange,
        count: Option<u32>,
    ) -> Vec<i32> {
        RedbEngine::get_all_keys(&db.store, store, range, count)
            .unwrap()
            .into_iter()
            .map(|raw_key| match encoding::deserialize(&raw_key).unwrap() {
                IndexedDBKeyType::Number(number) => number as i32,
                other => panic!("Expected numeric key, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn test_restart_keeps_data() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path.clone(), created);
        db.create_store("books", Some(KeyPath::String("isbn".to_string())), true)
            .unwrap();
        db.create_store("plain", None, false).unwrap();
        db.create_index(
            "books",
            "by_author".to_string(),
            KeyPath::String("author".to_string()),
            true,
            false,
        )
        .unwrap();
        let books = RedbEngine::object_store_by_name(&db.store, "books").unwrap();
        RedbEngine::put_item(
            &db.store,
            &books,
            IndexedDBKeyType::Number(1.0),
            vec![1, 2, 3],
            true,
            Some(2),
        )
        .unwrap();
        db.set_version(7).unwrap();
        drop(db);

        let db = open_engine(path, false);
        assert!(!db.created_db_path());
        assert_eq!(db.version().unwrap(), 7);
        assert_eq!(
            db.object_store_names().unwrap(),
            vec!["books".to_string(), "plain".to_string()]
        );
        assert_eq!(
            db.key_path("books"),
            Some(KeyPath::String("isbn".to_string()))
        );
        assert_eq!(db.key_path("plain"), None);
        assert_eq!(db.key_generator_current_number("books"), Some(2));
        assert_eq!(db.key_generator_current_number("plain"), None);
        let indexes = db.indexes("books").unwrap();
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].name, "by_author");
        assert!(indexes[0].unique);
        assert!(!indexes[0].multi_entry);
        let books = RedbEngine::object_store_by_name(&db.store, "books").unwrap();
        assert_eq!(
            RedbEngine::get_item(
                &db.store,
                &books,
                IndexedDBKeyRange::only(IndexedDBKeyType::Number(1.0))
            )
            .unwrap(),
            Some(vec![1, 2, 3])
        );
    }

    #[test]
    fn test_corrupt_rows_are_errors() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path, created);
        db.create_store("books", None, true).unwrap();
        let books = RedbEngine::object_store_by_name(&db.store, "books").unwrap();

        // A key that does not decode, next to one that does.
        RedbEngine::put_item(
            &db.store,
            &books,
            IndexedDBKeyType::Number(1.0),
            vec![1],
            true,
            None,
        )
        .unwrap();
        db.store
            .write(|txn| {
                txn.open_table(OBJECT_DATA)?
                    .insert((books.id, &[0x05][..]), &[2][..])?;
                Ok(())
            })
            .unwrap();
        let (sender, receiver) = generic_channel::channel().unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        db.process_transaction(
            KvsTransaction {
                mode: IndexedDBTxnMode::Readonly,
                requests: VecDeque::from(vec![KvsOperation {
                    store_name: "books".to_owned(),
                    operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetAllKeys {
                        callback: GenericCallback::new(move |result| {
                            let _ = sender.send(result.unwrap());
                        })
                        .unwrap(),
                        key_range: IndexedDBKeyRange::default(),
                        count: None,
                    }),
                }]),
            },
            Box::new(move || {
                let _ = done_tx.send(());
            }),
        );
        done_rx.recv().unwrap();
        let keys: Result<Vec<IndexedDBKeyType>, BackendError> = receiver.recv().unwrap();
        assert!(matches!(keys, Err(BackendError::DbErr(_))), "{keys:?}");

        // A row of `object_store` that does not decode.
        db.store
            .write(|txn| {
                txn.open_table(OBJECT_STORE)?
                    .insert(books.id, &[1, 2, 3][..])?;
                Ok(())
            })
            .unwrap();
        assert!(db.object_store_names().is_err());
        assert!(db.indexes("books").is_err());
        assert!(RedbEngine::object_store_by_name(&db.store, "books").is_err());
        assert_eq!(db.key_path("books"), None);
        assert_eq!(db.key_generator_current_number("books"), None);
        assert!(db.set_key_generator_current_number("books", 5).is_err());

        // A `database` row with bytes left over.
        db.store
            .write(|txn| {
                let mut database = txn.open_table(DATABASE)?;
                let mut record = super::database_record("origin", 3)?;
                record.push(0);
                database.insert(&b"test_db"[..], record.as_slice())?;
                Ok(())
            })
            .unwrap();
        assert!(db.version().is_err());
        assert!(db.set_version(4).is_err());

        // An index whose record is cut short.
        db.create_store("other", None, false).unwrap();
        db.create_index(
            "other",
            "i".to_string(),
            KeyPath::String("k".to_string()),
            false,
            false,
        )
        .unwrap();
        let other = RedbEngine::object_store_by_name(&db.store, "other").unwrap();
        db.store
            .write(|txn| {
                txn.open_table(OBJECT_STORE_INDEX)?
                    .insert((other.id, name_key("i").as_slice()), &[0][..])?;
                Ok(())
            })
            .unwrap();
        assert!(db.indexes("other").is_err());
    }

    #[test]
    fn test_names_come_in_sqlite_order() {
        // Servo's SQLite databases were UTF-16LE, and `SELECT name FROM object_store` read the
        // unique index on `name`: names in the order of their UTF-16LE bytes (checked against
        // SQLite 3.46 with the same schema and pragmas).
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path, created);
        let names = ["b", "a", "\u{e9}", "z", "\u{1F600}", "\u{FF21}", "A"];
        for name in names {
            db.create_store(name, None, false).unwrap();
            db.create_index(
                "b",
                name.to_string(),
                KeyPath::String("k".to_string()),
                false,
                false,
            )
            .unwrap();
        }
        let expected = ["\u{FF21}", "\u{1F600}", "A", "a", "b", "z", "\u{e9}"];
        assert_eq!(db.object_store_names().unwrap(), expected);
        let index_names: Vec<String> = db
            .indexes("b")
            .unwrap()
            .into_iter()
            .map(|index| index.name)
            .collect();
        assert_eq!(index_names, expected);
    }

    #[test]
    fn test_ranges_limits_and_index_renames() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path, created);
        db.create_store("s", None, false).unwrap();
        db.create_store("t", None, false).unwrap();
        let store = RedbEngine::object_store_by_name(&db.store, "s").unwrap();
        let other = RedbEngine::object_store_by_name(&db.store, "t").unwrap();
        for key in 1..=5 {
            RedbEngine::put_item(
                &db.store,
                &store,
                IndexedDBKeyType::Number(key as f64),
                vec![key as u8],
                false,
                None,
            )
            .unwrap();
        }
        // Rows of another store never show.
        RedbEngine::put_item(
            &db.store,
            &other,
            IndexedDBKeyType::Number(3.0),
            vec![0],
            false,
            None,
        )
        .unwrap();

        let number = |n: i32| Some(IndexedDBKeyType::Number(n as f64));
        // A range that ends before it starts holds nothing (and does not panic).
        assert!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::new(number(4), number(2), false, false),
                None
            )
            .is_empty()
        );
        assert!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::new(number(3), number(3), true, false),
                None
            )
            .is_empty()
        );
        assert!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::new(number(3), number(3), false, true),
                None
            )
            .is_empty()
        );
        assert_eq!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::new(number(3), number(3), false, false),
                None
            ),
            vec![3]
        );
        assert_eq!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::upper_bound(IndexedDBKeyType::Number(3.0), true),
                None
            ),
            vec![1, 2]
        );
        assert_eq!(
            number_keys(
                &db,
                &store,
                IndexedDBKeyRange::lower_bound(IndexedDBKeyType::Number(3.0), true),
                None
            ),
            vec![4, 5]
        );
        // LIMIT
        assert_eq!(
            number_keys(&db, &store, IndexedDBKeyRange::default(), Some(2)),
            vec![1, 2]
        );
        assert!(number_keys(&db, &store, IndexedDBKeyRange::default(), Some(0)).is_empty());
        assert_eq!(
            RedbEngine::count(&db.store, &store, IndexedDBKeyRange::default()).unwrap(),
            5
        );
        assert_eq!(
            RedbEngine::count(&db.store, &other, IndexedDBKeyRange::default()).unwrap(),
            1
        );
        // `get` is the first match in key order.
        assert_eq!(
            RedbEngine::get_item(
                &db.store,
                &store,
                IndexedDBKeyRange::lower_bound(IndexedDBKeyType::Number(2.0), true)
            )
            .unwrap(),
            Some(vec![3])
        );

        // Clearing one store leaves the other.
        RedbEngine::clear(&db.store, &store).unwrap();
        assert_eq!(
            RedbEngine::count(&db.store, &store, IndexedDBKeyRange::default()).unwrap(),
            0
        );
        assert_eq!(
            RedbEngine::count(&db.store, &other, IndexedDBKeyRange::default()).unwrap(),
            1
        );

        // Renames: a missing index is no error, a taken name is (UNIQUE), the same name is no change.
        let key_path = KeyPath::String("k".to_string());
        db.create_index("s", "one".to_string(), key_path.clone(), false, false)
            .unwrap();
        db.create_index("s", "two".to_string(), key_path.clone(), false, true)
            .unwrap();
        assert_eq!(
            db.create_index("s", "one".to_string(), key_path, false, false)
                .unwrap(),
            CreateObjectResult::AlreadyExists
        );
        RedbEngine::rename_index(&db.store, "s", "missing", "three", Commit::Durable).unwrap();
        assert!(RedbEngine::rename_index(&db.store, "s", "one", "two", Commit::Durable).is_err());
        RedbEngine::rename_index(&db.store, "s", "one", "one", Commit::Durable).unwrap();
        RedbEngine::rename_index(&db.store, "s", "two", "zero", Commit::Durable).unwrap();
        let indexes = db.indexes("s").unwrap();
        let names: Vec<&str> = indexes.iter().map(|index| index.name.as_str()).collect();
        assert_eq!(names, vec!["one", "zero"]);
        assert!(indexes[1].multi_entry);
        assert!(
            RedbEngine::rename_index(&db.store, "missing", "one", "x", Commit::Durable).is_err()
        );
        db.delete_index("s", "one".to_string()).unwrap();
        db.delete_index("s", "one".to_string()).unwrap();
        assert!(db.delete_index("missing", "one".to_string()).is_err());
        assert_eq!(db.indexes("s").unwrap().len(), 1);
        assert!(db.indexes("missing").is_err());

        // Deleting a store removes its indexes and rows only.
        db.delete_store("s").unwrap();
        assert!(db.delete_store("s").is_err());
        assert_eq!(object_data_rows(&db, store.id), 0);
        assert_eq!(object_data_rows(&db, other.id), 1);
        assert!(db.set_key_generator_current_number("s", 3).is_err());
        assert_eq!(db.object_store_names().unwrap(), vec!["t".to_string()]);
    }

    /// A readwrite transaction (put, get, create index) run through `run_transaction` with
    /// `make_durable`, and what it answered.
    fn run_with(
        db: &RedbEngine,
        mode: IndexedDBTxnMode,
        make_durable: impl FnOnce(&profile_store::Store) -> Result<(), profile_store::StoreError>,
    ) -> (
        Vec<&'static str>,
        Result<PutItemResult, BackendError>,
        Result<Option<Vec<u8>>, BackendError>,
        Option<BackendError>,
    ) {
        let events = Arc::new(std::sync::Mutex::new(vec![]));
        let (put_tx, put_rx) = generic_channel::channel().unwrap();
        let (get_tx, get_rx) = generic_channel::channel().unwrap();
        let (index_tx, index_rx) = generic_channel::channel().unwrap();
        let requests = VecDeque::from(vec![
            KvsOperation {
                store_name: "books".to_owned(),
                operation: AsyncOperation::ReadWrite(AsyncReadWriteOperation::PutItem {
                    callback: GenericCallback::new(move |r| {
                        let _ = put_tx.send(r.unwrap());
                    })
                    .unwrap(),
                    key: Some(IndexedDBKeyType::Number(1.0)),
                    value: vec![1],
                    should_overwrite: true,
                    key_generator_current_number: None,
                }),
            },
            KvsOperation {
                store_name: "books".to_owned(),
                operation: AsyncOperation::ReadOnly(AsyncReadOnlyOperation::GetItem {
                    callback: GenericCallback::new(move |r| {
                        let _ = get_tx.send(r.unwrap());
                    })
                    .unwrap(),
                    key_range: IndexedDBKeyRange::only(IndexedDBKeyType::Number(1.0)),
                }),
            },
            KvsOperation {
                store_name: "books".to_owned(),
                operation: AsyncOperation::Schema(AsyncSchemaOperation::CreateIndex {
                    callback: GenericCallback::new(move |r| {
                        let _ = index_tx.send(r.unwrap());
                    })
                    .unwrap(),
                    index_name: "by_title".to_owned(),
                    key_path: KeyPath::String("title".to_owned()),
                    unique: false,
                    multi_entry: false,
                }),
            },
        ]);
        let durable_events = events.clone();
        let complete_events = events.clone();
        super::run_transaction(
            &db.store,
            KvsTransaction { mode, requests },
            move |store| {
                durable_events.lock().unwrap().push("durable");
                make_durable(store)
            },
            Box::new(move || complete_events.lock().unwrap().push("complete")),
        );
        let events = events.lock().unwrap().clone();
        let put = put_rx.recv().unwrap();
        let get = get_rx.recv().unwrap();
        let index = index_rx
            .try_recv_timeout(std::time::Duration::from_millis(500))
            .ok();
        (events, put, get, index)
    }

    #[test]
    fn test_writing_transactions_are_durable_before_complete() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path, created);
        db.create_store("books", None, false).unwrap();

        for mode in [IndexedDBTxnMode::Readwrite, IndexedDBTxnMode::Versionchange] {
            let (events, put, get, index) = run_with(&db, mode, super::make_durable);
            assert_eq!(events, vec!["durable", "complete"]);
            assert_eq!(put, Ok(PutItemResult::Key(IndexedDBKeyType::Number(1.0))));
            assert_eq!(get, Ok(Some(vec![1])));
            assert_eq!(index, None);
            db.delete_index("books", "by_title".to_owned()).unwrap();
        }

        // Readonly transactions do not flush.
        let (events, _, get, _) = run_with(&db, IndexedDBTxnMode::Readonly, super::make_durable);
        assert_eq!(events, vec!["complete"]);
        assert_eq!(get, Ok(Some(vec![1])));
    }

    #[test]
    fn test_a_failed_flush_fails_the_writes() {
        let (_temp_dir, path, created, _proxy_map, _handle) = create_db("test_db".to_string());
        let db = open_engine(path, created);
        db.create_store("books", None, false).unwrap();

        let (events, put, get, index) = run_with(&db, IndexedDBTxnMode::Readwrite, |_| {
            Err(profile_store::StoreError::Io(std::io::Error::other(
                "no flush",
            )))
        });
        // `on_complete` is still called, after the attempt.
        assert_eq!(events, vec!["durable", "complete"]);
        assert!(matches!(put, Err(BackendError::DbErr(ref error)) if error.contains("no flush")));
        // A read keeps its answer; the schema change hears of the error.
        assert_eq!(get, Ok(Some(vec![1])));
        assert!(
            matches!(index, Some(BackendError::DbErr(ref error)) if error.contains("no flush"))
        );
    }
}
