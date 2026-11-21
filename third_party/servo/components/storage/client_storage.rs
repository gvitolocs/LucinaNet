/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
use std::collections::HashSet;
use std::fmt::{self, Debug};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::{fs, io, thread};

use log::{info, warn};
use profile_store::record::{Reader, Writer};
use profile_store::redb::{ReadableTable, TableDefinition, WriteTransaction};
use profile_store::{Store, StoreError};
use servo_base::generic_channel::{self, GenericReceiver, GenericSender};
use servo_base::id::{BrowsingContextId, WebViewId};
use servo_url::ImmutableOrigin;
use storage_traits::client_storage::{
    ClientStorageErrorr, ClientStorageThreadHandle, ClientStorageThreadMessage, Mode,
    StorageIdentifier, StorageProxyMap, StorageType,
};
use uuid::Uuid;

use crate::shared::{Commit, open_shared, write_or_roll_back};

/// <https://storage.spec.whatwg.org/#storage-quota>
/// The storage quota of a storage shelf is an implementation-defined conservative estimate of the
/// total amount of byttes it can hold. We use 10 GiB per shelf, matching Firefox's documented
/// limit (<https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria>).
const STORAGE_SHELF_QUOTA_BYTES: u64 = 10 * 1024 * 1024 * 1024;

trait RegistryEngine {
    type Error: Debug;
    fn create_database(
        &mut self,
        bottle_id: i64,
        name: String,
    ) -> Result<(PathBuf, bool), ClientStorageErrorr<Self::Error>>;
    fn delete_database(
        &mut self,
        bottle_id: i64,
        name: String,
    ) -> Result<(), ClientStorageErrorr<Self::Error>>;
    fn obtain_a_storage_bottle_map(
        &mut self,
        storage_type: StorageType,
        webview: Option<WebViewId>,
        storage_identifier: StorageIdentifier,
        origin: ImmutableOrigin,
        sender: &GenericSender<ClientStorageThreadMessage>,
    ) -> Result<StorageProxyMap, ClientStorageErrorr<Self::Error>>;
    fn persisted(&mut self, origin: ImmutableOrigin) -> Result<bool, String>;
    fn persist(
        &mut self,
        origin: ImmutableOrigin,
        permission_granted: bool,
    ) -> Result<bool, String>;
    fn estimate(&mut self, origin: ImmutableOrigin) -> Result<(u64, u64), String>;
}

// LucinaNet: Servo's registry was `reg.sqlite`, six tables with `INTEGER PRIMARY KEY` ids,
// UNIQUE constraints and foreign keys. Here each table is a redb table from its id to a record of
// its other columns (`profile_store::record`, in the order of Servo's columns), and each UNIQUE
// constraint is a key table from the unique columns to the id, kept in the same transaction as
// the row. Only `databases` rows are ever deleted, and their `directories` row with them (Servo's
// ON DELETE CASCADE). Text is stored as bytes and checked as UTF-8 when read: redb's own `&str`
// panics on bytes that are not UTF-8, and a corrupt file must give an error.

/// `sheds (id, storage_type, browsing_context)`: id → (storage_type, browsing_context or NULL).
const SHEDS: TableDefinition<u64, &[u8]> = TableDefinition::new("sheds");
/// `UNIQUE INDEX idx_sheds_local ON sheds(storage_type) WHERE browsing_context IS NULL`.
const SHEDS_WITHOUT_CONTEXT: TableDefinition<&[u8], u64> =
    TableDefinition::new("sheds_without_context");
/// `UNIQUE INDEX idx_sheds_session ON sheds(browsing_context) WHERE browsing_context IS NOT NULL`.
const SHEDS_BY_CONTEXT: TableDefinition<&[u8], u64> = TableDefinition::new("sheds_by_context");
/// `shelves (id, shed_id, origin)`: id → (shed_id, origin).
const SHELVES: TableDefinition<u64, &[u8]> = TableDefinition::new("shelves");
/// `UNIQUE (shed_id, origin)`.
const SHELVES_BY_SHED_ORIGIN: TableDefinition<(u64, &[u8]), u64> =
    TableDefinition::new("shelves_by_shed_origin");
/// `buckets (id, shelf_id, persisted, name, mode, expires)`: id → (shelf_id, persisted, name?,
/// mode?, expires?).
const BUCKETS: TableDefinition<u64, &[u8]> = TableDefinition::new("buckets");
/// `shelf_id ... UNIQUE`.
const BUCKETS_BY_SHELF: TableDefinition<u64, u64> = TableDefinition::new("buckets_by_shelf");
/// `bottles (id, bucket_id, identifier)`: id → (bucket_id, identifier).
const BOTTLES: TableDefinition<u64, &[u8]> = TableDefinition::new("bottles");
/// `UNIQUE (bucket_id, identifier)`.
const BOTTLES_BY_BUCKET_IDENTIFIER: TableDefinition<(u64, &[u8]), u64> =
    TableDefinition::new("bottles_by_bucket_identifier");
/// `databases (id, bottle_id, name)`: id → (bottle_id, name).
const DATABASES: TableDefinition<u64, &[u8]> = TableDefinition::new("databases");
/// `UNIQUE (bottle_id, name)`.
const DATABASES_BY_BOTTLE_NAME: TableDefinition<(u64, &[u8]), u64> =
    TableDefinition::new("databases_by_bottle_name");
/// `directories (id, database_id, path)`: id → (database_id, path).
const DIRECTORIES: TableDefinition<u64, &[u8]> = TableDefinition::new("directories");
/// `database_id ... UNIQUE`.
const DIRECTORIES_BY_DATABASE: TableDefinition<u64, u64> =
    TableDefinition::new("directories_by_database");
/// The last id given out in each table above, by table name: ids only grow, and one is never
/// given out twice.
const SEQUENCES: TableDefinition<&[u8], u64> = TableDefinition::new("sequences");

/// Why the registry failed: the store's error, or what rusqlite reported for the same case.
#[derive(Debug)]
pub(crate) enum RegistryError {
    Store(StoreError),
    /// rusqlite's `QueryReturnedNoRows`: a row that the query needed is not there.
    NoRow(&'static str),
    /// A FOREIGN KEY constraint: a row refers to one that is not there.
    ForeignKey(&'static str),
    /// A NULL read where a value was needed (rusqlite's `InvalidColumnType`).
    Null(&'static str),
    /// A path that is not UTF-8 (Servo: `InvalidParameterName("path")`).
    InvalidPath,
    /// Every id of a table is taken.
    IdsExhausted(&'static str),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::Store(error) => write!(f, "{error}"),
            RegistryError::NoRow(table) => write!(f, "Query returned no rows ({table})"),
            RegistryError::ForeignKey(column) => {
                write!(f, "FOREIGN KEY constraint failed ({column})")
            },
            RegistryError::Null(column) => write!(f, "NULL value in {column}"),
            RegistryError::InvalidPath => write!(f, "Invalid parameter name: path"),
            RegistryError::IdsExhausted(table) => write!(f, "no id left in {table}"),
        }
    }
}

impl From<StoreError> for RegistryError {
    fn from(error: StoreError) -> Self {
        RegistryError::Store(error)
    }
}

macro_rules! registry_error_from {
    ($($t:ty),*) => {$(
        impl From<$t> for RegistryError {
            fn from(error: $t) -> Self {
                RegistryError::Store(error.into())
            }
        }
    )*};
}
registry_error_from!(
    profile_store::redb::TableError,
    profile_store::redb::StorageError,
    profile_store::redb::TransactionError
);

/// An id as the registry's users see it: Servo's ids were SQLite's `i64`.
fn public_id(id: u64) -> Result<i64, RegistryError> {
    i64::try_from(id).map_err(|_| StoreError::corrupt(format!("id {id}")).into())
}

/// The next id of `table`: one more than the last one given out, as SQLite's
/// `INTEGER PRIMARY KEY` gives one more than the largest.
fn next_id(txn: &WriteTransaction, table: &'static str) -> Result<u64, RegistryError> {
    let mut sequences = txn.open_table(SEQUENCES)?;
    let last = sequences
        .get(table.as_bytes())?
        .map(|last| last.value())
        .unwrap_or(0);
    let id = last
        .checked_add(1)
        .filter(|id| *id <= i64::MAX as u64)
        .ok_or(RegistryError::IdsExhausted(table))?;
    sequences.insert(table.as_bytes(), id)?;
    Ok(id)
}

fn shed_record(storage_type: &str, browsing_context: Option<&str>) -> Result<Vec<u8>, StoreError> {
    Ok(Writer::new()
        .str(storage_type)?
        .opt_str(browsing_context)?
        .finish())
}

fn read_shed(bytes: &[u8]) -> Result<(String, Option<String>), StoreError> {
    let mut reader = Reader::new(bytes);
    let storage_type = reader.string()?;
    let browsing_context = reader.opt_string()?;
    reader.end()?;
    Ok((storage_type, browsing_context))
}

/// A row of `shelves`, `bottles`, `databases` or `directories`: the id of the row it belongs to,
/// and one text column.
fn pair_record(owner: u64, text: &str) -> Result<Vec<u8>, StoreError> {
    Ok(Writer::new().u64(owner)?.str(text)?.finish())
}

fn read_pair(bytes: &[u8]) -> Result<(u64, String), StoreError> {
    let mut reader = Reader::new(bytes);
    let owner = reader.u64()?;
    let text = reader.string()?;
    reader.end()?;
    Ok((owner, text))
}

/// A row of `buckets`.
struct Bucket {
    shelf_id: u64,
    persisted: bool,
    name: Option<String>,
    mode: Option<String>,
    expires: Option<String>,
}

impl Bucket {
    fn record(&self) -> Result<Vec<u8>, StoreError> {
        Ok(Writer::new()
            .u64(self.shelf_id)?
            .bool(self.persisted)?
            .opt_str(self.name.as_deref())?
            .opt_str(self.mode.as_deref())?
            .opt_str(self.expires.as_deref())?
            .finish())
    }

    fn read(bytes: &[u8]) -> Result<Bucket, StoreError> {
        let mut reader = Reader::new(bytes);
        let bucket = Bucket {
            shelf_id: reader.u64()?,
            persisted: reader.bool()?,
            name: reader.opt_string()?,
            mode: reader.opt_string()?,
            expires: reader.opt_string()?,
        };
        reader.end()?;
        Ok(bucket)
    }
}

/// Creates every table of the registry, so that a read transaction finds them.
fn create_tables(store: &Store) -> Result<(), StoreError> {
    store.write(|txn| {
        txn.open_table(SHEDS)?;
        txn.open_table(SHEDS_WITHOUT_CONTEXT)?;
        txn.open_table(SHEDS_BY_CONTEXT)?;
        txn.open_table(SHELVES)?;
        txn.open_table(SHELVES_BY_SHED_ORIGIN)?;
        txn.open_table(BUCKETS)?;
        txn.open_table(BUCKETS_BY_SHELF)?;
        txn.open_table(BOTTLES)?;
        txn.open_table(BOTTLES_BY_BUCKET_IDENTIFIER)?;
        txn.open_table(DATABASES)?;
        txn.open_table(DATABASES_BY_BOTTLE_NAME)?;
        txn.open_table(DIRECTORIES)?;
        txn.open_table(DIRECTORIES_BY_DATABASE)?;
        txn.open_table(SEQUENCES)?;
        Ok(())
    })
}

/// The last two parts of a database directory's path (`<cluster>/<uuid>`), by which a directory
/// on disk is matched to its row: matching whole paths would take every directory for an orphan
/// once the profile has moved.
fn directory_name(path: &Path) -> Option<(String, String)> {
    let name = path.file_name()?.to_str()?.to_owned();
    let cluster = path.parent()?.file_name()?.to_str()?.to_owned();
    Some((cluster, name))
}

/// What `remove_orphan_directories` did.
#[derive(Debug, Default, PartialEq)]
struct Sweep {
    /// Directories without a row, removed.
    removed: usize,
    /// Directories without a row, kept: they hold something IndexedDB did not put there, or the
    /// registry is new and so knows none of them.
    kept: usize,
}

/// Whether a database directory holds nothing but what this engine's IndexedDB writes into one
/// (`indexeddb.redb`), so that removing it can lose nothing else.
fn holds_only_indexeddb_files(path: &Path) -> io::Result<bool> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let is_store_file = entry.file_type()?.is_file() &&
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("indexeddb.redb"));
        if !is_store_file {
            return Ok(false);
        }
    }
    Ok(true)
}

/// LucinaNet: removes the directories under `<base>/bottles/*/*` that no `directories` row
/// refers to. Servo committed the row before creating the directory; here the directory is
/// created first (see `create_database`), so a crash between the two leaves a directory without
/// a row, which nothing will ever open: this is where it goes.
///
/// Nothing is removed when the registry file was created by this open (`registry_is_new`): its
/// directories then belong to a registry this one does not know (Servo's `reg.sqlite`, or a
/// registry lost), not to a crash. Nor is a directory that holds anything but `indexeddb.redb`
/// files.
fn remove_orphan_directories(
    store: &Store,
    base_dir: &Path,
    registry_is_new: bool,
) -> Result<Sweep, StoreError> {
    let mut referenced = HashSet::new();
    {
        let txn = store.read()?;
        let directories = txn.open_table(DIRECTORIES)?;
        for row in directories.iter()? {
            let (_, record) = row?;
            let (_, path) = read_pair(record.value())?;
            if let Some(name) = directory_name(Path::new(&path)) {
                referenced.insert(name);
            }
        }
    }
    let bottles = base_dir.join("bottles");
    let clusters = match fs::read_dir(&bottles) {
        Ok(clusters) => clusters,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Sweep::default()),
        Err(error) => return Err(error.into()),
    };
    let mut orphans = vec![];
    for cluster in clusters {
        let cluster = cluster?;
        if !cluster.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(cluster.path())? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if directory_name(&path).is_some_and(|name| referenced.contains(&name)) {
                continue;
            }
            orphans.push(path);
        }
    }

    let mut sweep = Sweep::default();
    if registry_is_new {
        sweep.kept = orphans.len();
        if sweep.kept > 0 {
            warn!(
                "ClientStorage: the registry is new but {} database directories exist under {bottles:?}: \
                 kept, none removed",
                sweep.kept
            );
        }
        return Ok(sweep);
    }
    for path in orphans {
        if holds_only_indexeddb_files(&path)? {
            fs::remove_dir_all(&path)?;
            sweep.removed += 1;
        } else {
            warn!(
                "ClientStorage: {path:?} has no registry row but holds more than an IndexedDB \
                 database: kept"
            );
            sweep.kept += 1;
        }
    }
    Ok(sweep)
}

/// Makes the entries of a new directory's parents durable, from its parent up to `base_dir`.
fn sync_parents(path: &Path, base_dir: &Path) -> io::Result<()> {
    let mut parent = path.parent();
    while let Some(dir) = parent {
        File::open(dir)?.sync_all()?;
        if dir == base_dir {
            break;
        }
        parent = dir.parent();
    }
    Ok(())
}

pub(crate) struct RedbEngine {
    store: Arc<Store>,
    base_dir: PathBuf,
}

impl RedbEngine {
    pub(crate) fn new(base_dir: PathBuf) -> Result<Self, StoreError> {
        let db_path = base_dir.join("reg.redb");
        let registry_is_new = !db_path.exists();
        let store = open_shared(&db_path, |store| {
            create_tables(store)?;
            match remove_orphan_directories(store, &base_dir, registry_is_new) {
                Ok(sweep) => info!(
                    "ClientStorage: removed {} database directories without a registry row, kept {}",
                    sweep.removed, sweep.kept
                ),
                Err(error) => {
                    warn!("ClientStorage: could not look for orphan directories: {error}")
                },
            }
            Ok(())
        })?;
        Ok(RedbEngine { store, base_dir })
    }

    pub(crate) fn memory() -> Result<Self, StoreError> {
        let store = Store::memory()?;
        create_tables(&store)?;
        Ok(RedbEngine {
            store: Arc::new(store),
            base_dir: PathBuf::new(),
        })
    }
}

fn ensure_storage_shed(
    storage_type: &StorageType,
    browsing_context: Option<String>,
    txn: &WriteTransaction,
) -> Result<u64, RegistryError> {
    match browsing_context {
        Some(browsing_context) => {
            // INSERT ... ON CONFLICT DO NOTHING: the session shed of a browsing context is unique.
            let existing = txn
                .open_table(SHEDS_BY_CONTEXT)?
                .get(browsing_context.as_bytes())?
                .map(|id| id.value());
            let id = match existing {
                Some(id) => id,
                None => {
                    let id = next_id(txn, "sheds")?;
                    let record = shed_record(storage_type.as_str(), Some(&browsing_context))?;
                    txn.open_table(SHEDS)?.insert(id, record.as_slice())?;
                    txn.open_table(SHEDS_BY_CONTEXT)?
                        .insert(browsing_context.as_bytes(), id)?;
                    id
                },
            };
            // SELECT id FROM sheds WHERE storage_type = ?1 AND browsing_context = ?2;
            let sheds = txn.open_table(SHEDS)?;
            let record = sheds.get(id)?.ok_or(RegistryError::NoRow("sheds"))?;
            let (row_type, _) = read_shed(record.value())?;
            if row_type != storage_type.as_str() {
                return Err(RegistryError::NoRow("sheds"));
            }
            Ok(id)
        },
        None => {
            let existing = txn
                .open_table(SHEDS_WITHOUT_CONTEXT)?
                .get(storage_type.as_str().as_bytes())?
                .map(|id| id.value());
            match existing {
                Some(id) => Ok(id),
                None => {
                    let id = next_id(txn, "sheds")?;
                    let record = shed_record(storage_type.as_str(), None)?;
                    txn.open_table(SHEDS)?.insert(id, record.as_slice())?;
                    txn.open_table(SHEDS_WITHOUT_CONTEXT)?
                        .insert(storage_type.as_str().as_bytes(), id)?;
                    Ok(id)
                },
            }
        },
    }
}

/// <https://storage.spec.whatwg.org/#create-a-storage-bucket>
fn create_a_storage_bucket(
    shelf_id: u64,
    storage_type: StorageType,
    txn: &WriteTransaction,
) -> Result<u64, RegistryError> {
    // Step 1. Let bucket be null.
    // Step 2. If type is "local", then set bucket to a new local storage bucket.
    // Step 3. Otherwise:
    // Step 3.1. Assert: type is "session".
    // Step 3.2. Set bucket to a new session storage bucket.
    // (Servo: INSERT ... ON CONFLICT(shelf_id) DO UPDATE ... RETURNING id: the existing bucket of
    // the shelf, unchanged, or a new one.)
    let existing = txn
        .open_table(BUCKETS_BY_SHELF)?
        .get(shelf_id)?
        .map(|id| id.value());
    let bucket_id = match existing {
        Some(id) => id,
        None => {
            let id = next_id(txn, "buckets")?;
            let mode = match storage_type {
                StorageType::Local => Some(Mode::default().as_str().to_owned()),
                StorageType::Session => None,
            };
            let bucket = Bucket {
                shelf_id,
                persisted: false,
                name: None,
                mode,
                expires: None,
            };
            txn.open_table(BUCKETS)?
                .insert(id, bucket.record()?.as_slice())?;
            txn.open_table(BUCKETS_BY_SHELF)?.insert(shelf_id, id)?;
            id
        },
    };

    // Step 4. For each endpoint of registered storage endpoints whose types contain type,
    // set bucket’s bottle map[endpoint’s identifier] to
    // a new storage bottle whose quota is endpoint’s quota.

    // <https://storage.spec.whatwg.org/#registered-storage-endpoints>
    let registered_endpoints = match storage_type {
        StorageType::Local => vec![
            StorageIdentifier::Caches,
            StorageIdentifier::IndexedDB,
            StorageIdentifier::LocalStorage,
            StorageIdentifier::ServiceWorkerRegistrations,
        ],
        StorageType::Session => vec![StorageIdentifier::SessionStorage],
    };

    for identifier in registered_endpoints {
        // INSERT ... ON CONFLICT(bucket_id, identifier) DO NOTHING;
        let key = (bucket_id, identifier.as_str().as_bytes());
        let exists = txn
            .open_table(BOTTLES_BY_BUCKET_IDENTIFIER)?
            .get(key)?
            .is_some();
        if !exists {
            let id = next_id(txn, "bottles")?;
            let record = pair_record(bucket_id, identifier.as_str())?;
            txn.open_table(BOTTLES)?.insert(id, record.as_slice())?;
            txn.open_table(BOTTLES_BY_BUCKET_IDENTIFIER)?
                .insert(key, id)?;
        }
    }

    // Step 5. Return bucket.
    Ok(bucket_id)
}

/// <https://storage.spec.whatwg.org/#create-a-storage-shelf>
fn create_a_storage_shelf(
    shed: u64,
    origin: &ImmutableOrigin,
    storage_type: StorageType,
    txn: &WriteTransaction,
) -> Result<StorageShelf, RegistryError> {
    // To create a storage shelf, given a storage type type, run these steps:
    // Step 1. Let shelf be a new storage shelf.
    // Step 2.  Set shelf’s bucket map["default"] to the result of running create a storage bucket with type.
    // (Servo: INSERT ... ON CONFLICT(shed_id, origin) DO UPDATE ... RETURNING id.)
    let origin = origin.ascii_serialization().into_owned();
    let key = (shed, origin.as_bytes());
    let existing = txn
        .open_table(SHELVES_BY_SHED_ORIGIN)?
        .get(key)?
        .map(|id| id.value());
    let shelf_id = match existing {
        Some(id) => id,
        None => {
            let id = next_id(txn, "shelves")?;
            let record = pair_record(shed, &origin)?;
            txn.open_table(SHELVES)?.insert(id, record.as_slice())?;
            txn.open_table(SHELVES_BY_SHED_ORIGIN)?.insert(key, id)?;
            id
        },
    };

    // Step 3. Return shelf.
    Ok(StorageShelf {
        default_bucket_id: create_a_storage_bucket(shelf_id, storage_type, txn)?,
    })
}

/// <https://storage.spec.whatwg.org/#obtain-a-storage-shelf>
fn obtain_a_storage_shelf(
    shed: u64,
    origin: &ImmutableOrigin,
    storage_type: StorageType,
    txn: &WriteTransaction,
) -> Result<StorageShelf, RegistryError> {
    create_a_storage_shelf(shed, origin, storage_type, txn)
}

/// <https://storage.spec.whatwg.org/#storage-shelf>
///
/// A storage shelf exists for each storage key within a storage shed. It holds a bucket map, which
/// is a map of strings to storage buckets.
struct StorageShelf {
    default_bucket_id: u64,
}

/// <https://storage.spec.whatwg.org/#obtain-a-local-storage-shelf>
///
/// To obtain a local storage shelf, given an environment settings object environment, return the
/// result of running obtain a storage shelf with the user agent’s storage shed, environment, and
/// "local".
fn obtain_a_local_storage_shelf(
    origin: &ImmutableOrigin,
    txn: &WriteTransaction,
) -> Result<StorageShelf, String> {
    if !origin.is_tuple() {
        return Err("Storage is unavailable for opaque origins".to_owned());
    }

    let shed =
        ensure_storage_shed(&StorageType::Local, None, txn).map_err(|error| error.to_string())?;
    obtain_a_storage_shelf(shed, origin, StorageType::Local, txn).map_err(|error| error.to_string())
}

fn read_bucket(bucket_id: u64, txn: &WriteTransaction) -> Result<Bucket, RegistryError> {
    let buckets = txn.open_table(BUCKETS)?;
    let record = buckets
        .get(bucket_id)?
        .ok_or(RegistryError::NoRow("buckets"))?;
    Ok(Bucket::read(record.value())?)
}

/// <https://storage.spec.whatwg.org/#bucket-mode>
///
/// A local storage bucket has a mode, which is "best-effort" or "persistent". It is initially
/// "best-effort".
fn bucket_mode(bucket_id: u64, txn: &WriteTransaction) -> Result<Mode, RegistryError> {
    let mode = read_bucket(bucket_id, txn)?
        .mode
        .ok_or(RegistryError::Null("buckets.mode"))?;
    Ok(Mode::from_str(&mode).unwrap_or_default())
}

/// <https://storage.spec.whatwg.org/#dom-storagemanager-persist>
///
/// Set bucket’s mode to "persistent".
fn set_bucket_mode(
    bucket_id: u64,
    mode: Mode,
    txn: &WriteTransaction,
) -> Result<(), RegistryError> {
    // UPDATE buckets SET mode = ?1, persisted = ?2 WHERE id = ?3; (no row: nothing changes)
    let mut bucket = match read_bucket(bucket_id, txn) {
        Ok(bucket) => bucket,
        Err(RegistryError::NoRow(_)) => return Ok(()),
        Err(error) => return Err(error),
    };
    bucket.persisted = matches!(mode, Mode::Persistent);
    bucket.mode = Some(mode.as_str().to_owned());
    txn.open_table(BUCKETS)?
        .insert(bucket_id, bucket.record()?.as_slice())?;
    Ok(())
}

/// The ids in a key table `(owner, text) → id` whose owner is `owner`, in key order.
fn ids_owned_by(
    table: &impl ReadableTable<(u64, &'static [u8]), u64>,
    owner: u64,
) -> Result<Vec<u64>, RegistryError> {
    let mut ids = vec![];
    for row in table.range((owner, &[][..])..)? {
        let (key, id) = row?;
        if key.value().0 != owner {
            break;
        }
        ids.push(id.value());
    }
    Ok(ids)
}

/// The path in the `directories` row of a database, if it has one.
fn directory_of(database_id: u64, txn: &WriteTransaction) -> Result<Option<String>, RegistryError> {
    let Some(directory_id) = txn
        .open_table(DIRECTORIES_BY_DATABASE)?
        .get(database_id)?
        .map(|id| id.value())
    else {
        return Ok(None);
    };
    let directories = txn.open_table(DIRECTORIES)?;
    let record = directories
        .get(directory_id)?
        .ok_or_else(|| StoreError::corrupt(format!("directory {directory_id} without its row")))?;
    Ok(Some(read_pair(record.value())?.1))
}

/// <https://storage.spec.whatwg.org/#storage-usage>
///
/// The storage usage of a storage shelf is an implementation-defined rough estimate of the amount
/// of bytes used by it.
///
/// This cannot be an exact amount as user agents might, and are encouraged to, use deduplication,
/// compression, and other techniques that obscure exactly how much bytes a storage shelf uses.
///
/// LucinaNet: in two steps, the paths of the bucket's database directories read in the
/// registry's transaction ([`database_paths_of_bucket`]), then the walk of those directories
/// after it ([`storage_usage`]), so the registry is not locked while the disk is read. Servo did
/// both inside its transaction.
fn database_paths_of_bucket(bucket_id: u64, txn: &WriteTransaction) -> Result<Vec<String>, String> {
    // Servo: SELECT directories.path FROM directories JOIN databases JOIN bottles
    // WHERE bottles.bucket_id = ?1;
    (|| -> Result<Vec<String>, RegistryError> {
        let bottles = ids_owned_by(&txn.open_table(BOTTLES_BY_BUCKET_IDENTIFIER)?, bucket_id)?;
        let mut databases = vec![];
        {
            let by_bottle = txn.open_table(DATABASES_BY_BOTTLE_NAME)?;
            for bottle in bottles {
                databases.extend(ids_owned_by(&by_bottle, bottle)?);
            }
        }
        let mut paths = vec![];
        for database in databases {
            paths.extend(directory_of(database, txn)?);
        }
        Ok(paths)
    })()
    .map_err(|error| error.to_string())
}

/// The bytes under the directories at `paths`.
fn storage_usage(paths: Vec<String>) -> Result<u64, String> {
    let mut usage = 0_u64;
    for path in paths {
        usage += directory_size(&PathBuf::from(path))?;
    }
    Ok(usage)
}

/// <https://storage.spec.whatwg.org/#storage-quota>
///
/// The storage quota of a storage shelf is an implementation-defined conservative estimate of the
/// total amount of bytes it can hold. This amount should be less than the total storage space on
/// the device. It must not be a function of the available storage space on the device.
///
/// User agents are strongly encouraged to consider navigation frequency, recency of visits,
/// bookmarking, and permission for "persistent-storage" when determining quotas.
///
/// Directly or indirectly revealing available storage space can lead to fingerprinting and leaking
/// information outside the scope of the origin involved.
fn storage_quota_for_bucket(_bucket_id: u64, _txn: &WriteTransaction) -> Result<u64, String> {
    Ok(STORAGE_SHELF_QUOTA_BYTES)
}

/// <https://storage.spec.whatwg.org/#storage-usage>
///
/// The storage usage of a storage shelf is an implementation-defined rough estimate of the amount
/// of bytes used by it.
fn directory_size(path: &PathBuf) -> Result<u64, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }

    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut size = 0_u64;
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        size += directory_size(&entry.path())?;
    }
    Ok(size)
}

/// The bottle with this id, as the FOREIGN KEY of `databases.bottle_id` required.
fn require_bottle(bottle_id: i64, txn: &WriteTransaction) -> Result<u64, RegistryError> {
    let missing = RegistryError::ForeignKey("databases.bottle_id");
    let Ok(bottle_id) = u64::try_from(bottle_id) else {
        return Err(missing);
    };
    if txn.open_table(BOTTLES)?.get(bottle_id)?.is_none() {
        return Err(missing);
    }
    Ok(bottle_id)
}

fn internal(error: impl Into<RegistryError>) -> ClientStorageErrorr<RegistryError> {
    ClientStorageErrorr::Internal(error.into())
}

impl RedbEngine {
    /// The directory of an existing database, read without a write transaction (whose durable
    /// commit `create_database` would otherwise pay on every open). Any failure here leaves the
    /// answer to the write transaction, which reports it.
    fn existing_directory(&self, bottle_id: i64, name: &str) -> Option<String> {
        let bottle_id = u64::try_from(bottle_id).ok()?;
        let txn = self.store.read().ok()?;
        let database_id = txn
            .open_table(DATABASES_BY_BOTTLE_NAME)
            .ok()?
            .get((bottle_id, name.as_bytes()))
            .ok()??
            .value();
        let directory_id = txn
            .open_table(DIRECTORIES_BY_DATABASE)
            .ok()?
            .get(database_id)
            .ok()??
            .value();
        let directories = txn.open_table(DIRECTORIES).ok()?;
        let record = directories.get(directory_id).ok()??;
        read_pair(record.value()).ok().map(|(_, path)| path)
    }
}

/// What `create_database` found or made, inside its transaction.
enum DatabaseDirectory {
    Existing(String),
    Created(PathBuf),
}

impl RegistryEngine for RedbEngine {
    type Error = RegistryError;

    /// Create a database for the indexedDB endpoint.
    fn create_database(
        &mut self,
        bottle_id: i64,
        name: String,
    ) -> Result<(PathBuf, bool), ClientStorageErrorr<Self::Error>> {
        // LucinaNet: Servo committed the `databases` and `directories` rows, then created the
        // directory. A crash in between left a row whose directory is missing, and every later
        // call returned that path with `created = false`, which IndexedDB then failed to open.
        // Here the directory is created (and its entry synced) inside the transaction, before the
        // rows are committed, durably; if it cannot be created nothing is committed. A crash
        // after the directory and before the commit leaves a directory without a row, removed at
        // the next start (`remove_orphan_directories`).
        if let Some(path) = self.existing_directory(bottle_id, &name) {
            return Ok((PathBuf::from(path), false));
        }

        let base_dir = self.base_dir.clone();
        let mut created_dir = None;
        let result = write_or_roll_back(&self.store, Commit::Durable, |txn| {
            let bottle_id = require_bottle(bottle_id, txn)?;

            // INSERT INTO databases ... ON CONFLICT(bottle_id, name) DO UPDATE ... RETURNING id;
            let key = (bottle_id, name.as_bytes());
            let existing = txn
                .open_table(DATABASES_BY_BOTTLE_NAME)
                .map_err(internal)?
                .get(key)
                .map_err(internal)?
                .map(|id| id.value());
            let database_id = match existing {
                Some(id) => id,
                None => {
                    let id = next_id(txn, "databases")?;
                    let record = pair_record(bottle_id, &name).map_err(internal)?;
                    txn.open_table(DATABASES)
                        .map_err(internal)?
                        .insert(id, record.as_slice())
                        .map_err(internal)?;
                    txn.open_table(DATABASES_BY_BOTTLE_NAME)
                        .map_err(internal)?
                        .insert(key, id)
                        .map_err(internal)?;
                    id
                },
            };

            if let Some(path) = directory_of(database_id, txn)? {
                return Ok(DatabaseDirectory::Existing(path));
            }

            let dir = Uuid::new_v4().to_string();
            let cluster = dir.chars().last().map(String::from).unwrap_or_default();
            let path = base_dir.join("bottles").join(cluster).join(dir);

            let path_str = path
                .to_str()
                .ok_or(ClientStorageErrorr::Internal(RegistryError::InvalidPath))?
                .to_owned();

            fs::create_dir_all(&path).map_err(|_| ClientStorageErrorr::DirectoryCreationFailed)?;
            created_dir = Some(path.clone());
            sync_parents(&path, &base_dir)
                .map_err(|_| ClientStorageErrorr::DirectoryCreationFailed)?;

            let id = next_id(txn, "directories")?;
            let record = pair_record(database_id, &path_str).map_err(internal)?;
            txn.open_table(DIRECTORIES)
                .map_err(internal)?
                .insert(id, record.as_slice())
                .map_err(internal)?;
            txn.open_table(DIRECTORIES_BY_DATABASE)
                .map_err(internal)?
                .insert(database_id, id)
                .map_err(internal)?;
            Ok(DatabaseDirectory::Created(path))
        });
        let failed = !matches!(result, Ok(Ok(_)));
        if let Some(path) = created_dir.filter(|_| failed) {
            // No row refers to it: do not leave it for the next start.
            if let Err(error) = fs::remove_dir_all(&path) {
                warn!("ClientStorage: could not remove {path:?} after a failed create: {error}");
            }
        }
        match result {
            Ok(Ok(DatabaseDirectory::Existing(path))) => Ok((PathBuf::from(path), false)),
            Ok(Ok(DatabaseDirectory::Created(path))) => Ok((path, true)),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(ClientStorageErrorr::Internal(error.into())),
        }
    }

    /// Delete a database for the indexedDB endpoint.
    fn delete_database(
        &mut self,
        bottle_id: i64,
        name: String,
    ) -> Result<(), ClientStorageErrorr<Self::Error>> {
        let result = write_or_roll_back(&self.store, Commit::Durable, |txn| {
            let Ok(bottle_id) = u64::try_from(bottle_id) else {
                return Err(ClientStorageErrorr::Internal(RegistryError::NoRow(
                    "databases",
                )));
            };
            let key = (bottle_id, name.as_bytes());

            // SELECT id FROM databases WHERE bottle_id = ?1 AND name = ?2;
            let database_id = txn
                .open_table(DATABASES_BY_BOTTLE_NAME)
                .map_err(internal)?
                .get(key)
                .map_err(internal)?
                .map(|id| id.value())
                .ok_or(RegistryError::NoRow("databases"))?;

            // SELECT path FROM directories WHERE database_id = ?1;
            let path =
                directory_of(database_id, txn)?.ok_or(RegistryError::NoRow("directories"))?;

            // DELETE FROM databases WHERE bottle_id = ?1 AND name = ?2;
            let removed = txn
                .open_table(DATABASES)
                .map_err(internal)?
                .remove(database_id)
                .map_err(internal)?
                .is_some();
            txn.open_table(DATABASES_BY_BOTTLE_NAME)
                .map_err(internal)?
                .remove(key)
                .map_err(internal)?;
            if !removed {
                return Err(ClientStorageErrorr::DatabaseDoesNotExist);
            }
            // Servo's ON DELETE CASCADE of `directories`.
            if let Some(directory_id) = txn
                .open_table(DIRECTORIES_BY_DATABASE)
                .map_err(internal)?
                .remove(database_id)
                .map_err(internal)?
                .map(|id| id.value())
            {
                txn.open_table(DIRECTORIES)
                    .map_err(internal)?
                    .remove(directory_id)
                    .map_err(internal)?;
            }
            Ok(path)
        });
        let path = match result {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => return Err(error),
            Err(error) => return Err(ClientStorageErrorr::Internal(error.into())),
        };

        // Delete the directory on disk, once the removal of its row is durable (as Servo).
        fs::remove_dir_all(&path).map_err(|_| ClientStorageErrorr::DirectoryDeletionFailed)?;

        Ok(())
    }

    /// <https://storage.spec.whatwg.org/#obtain-a-storage-bottle-map>
    fn obtain_a_storage_bottle_map(
        &mut self,
        storage_type: StorageType,
        webview: Option<WebViewId>,
        storage_identifier: StorageIdentifier,
        origin: ImmutableOrigin,
        sender: &GenericSender<ClientStorageThreadMessage>,
    ) -> Result<StorageProxyMap, ClientStorageErrorr<Self::Error>> {
        // Step 1. Let shed be null.
        // Step 3: Otherwise:
        // Step 3.1: Assert: type is "session".
        let browsing_context = match storage_type {
            StorageType::Local => None,
            StorageType::Session => {
                let Some(webview) = webview else {
                    debug_assert!(false, "Session storage is only available on Window.");
                    return Err(ClientStorageErrorr::SessionStorageRequiresWindow);
                };
                Some(Into::<BrowsingContextId>::into(webview).to_string())
            },
        };

        let result = write_or_roll_back(&self.store, Commit::Soon, |txn| {
            // Step 2. If type is "local", then set shed to the user agent’s storage shed.
            // Step 3.2: Set shed to environment’s global object’s associated Document’s
            // node navigable’s traversable navigable’s storage shed.
            // Note: using the browsing context of the webview as the traversable navigable.
            let shed_id = ensure_storage_shed(&storage_type, browsing_context, txn)?;

            // Step 4. Let shelf be the result of running obtain a storage shelf, with shed,
            // environment, and type.
            // Step 5. If shelf is failure, then return failure.
            let shelf = obtain_a_storage_shelf(shed_id, &origin, storage_type, txn)?;

            // Step 6. Let bucket be shelf’s bucket map["default"].
            let bucket_id = shelf.default_bucket_id;

            // SELECT id FROM bottles WHERE bucket_id = ?1 AND identifier = ?2;
            let bottle_id = txn
                .open_table(BOTTLES_BY_BUCKET_IDENTIFIER)?
                .get((bucket_id, storage_identifier.as_str().as_bytes()))?
                .map(|id| id.value())
                .ok_or(RegistryError::NoRow("bottles"))?;
            public_id(bottle_id)
        });
        let bottle_id = match result {
            Ok(Ok(bottle_id)) => bottle_id,
            Ok(Err(error)) => return Err(ClientStorageErrorr::Internal(error)),
            Err(error) => return Err(ClientStorageErrorr::Internal(error.into())),
        };

        // Step 7. Let bottle be bucket’s bottle map[identifier].

        // Step 8. Let proxyMap be a new storage proxy map whose backing map is bottle’s map.
        // Step 9. Append proxyMap to bottle’s proxy map reference set.
        // Step 10. Return proxyMap.
        Ok(StorageProxyMap {
            bottle_id,
            handle: ClientStorageThreadHandle::new(sender.clone()),
        })
    }

    fn persisted(&mut self, origin: ImmutableOrigin) -> Result<bool, String> {
        // Servo committed only when the mode was "persistent" (`is_ok_and(..) && tx.commit()`):
        // otherwise the shelf made on the way was rolled back. `Err(None)` rolls back the same.
        let result = write_or_roll_back(&self.store, Commit::Soon, |txn| {
            // <https://storage.spec.whatwg.org/#dom-storagemanager-persisted>
            // Let shelf be the result of running obtain a local storage shelf with this’s relevant
            // settings object.
            let shelf = obtain_a_local_storage_shelf(&origin, txn).map_err(Some)?;

            // Let persisted be true if shelf’s bucket map["default"]'s mode is "persistent";
            // otherwise false.
            // It will be false when there’s an internal error.
            match bucket_mode(shelf.default_bucket_id, txn) {
                Ok(Mode::Persistent) => Ok(()),
                _ => Err(None),
            }
        });

        match result {
            Ok(Ok(())) => Ok(true),
            Ok(Err(Some(error))) => Err(error),
            Ok(Err(None)) | Err(_) => Ok(false),
        }
    }

    fn persist(
        &mut self,
        origin: ImmutableOrigin,
        permission_granted: bool,
    ) -> Result<bool, String> {
        // LucinaNet: a bucket made persistent is on disk before `persist` returns true (Servo's
        // SQLite commit survived a crash of the process; the store's non-durable commit may not).
        let commit = if permission_granted {
            Commit::Durable
        } else {
            Commit::Soon
        };
        let result = write_or_roll_back(&self.store, commit, |txn| {
            // <https://storage.spec.whatwg.org/#dom-storagemanager-persist>
            // Let shelf be the result of running obtain a local storage shelf with this’s relevant
            // settings object.
            let shelf = obtain_a_local_storage_shelf(&origin, txn)?;

            // Let bucket be shelf’s bucket map["default"].
            let bucket_id = shelf.default_bucket_id;

            // Let persisted be true if bucket’s mode is "persistent"; otherwise false.
            // It will be false when there’s an internal error.
            let mut persisted =
                bucket_mode(bucket_id, txn).is_ok_and(|mode| mode == Mode::Persistent);

            // If persisted is false and permission is "granted", then:
            // Set bucket’s mode to "persistent".
            // If there was no internal error, then set persisted to true.
            if !persisted && permission_granted {
                persisted = set_bucket_mode(bucket_id, Mode::Persistent, txn).is_ok();
            }
            Ok(persisted)
        });

        match result {
            Ok(Ok(persisted)) => Ok(persisted),
            Ok(Err(error)) => Err(error),
            // The commit failed.
            Err(_) => Ok(false),
        }
    }

    fn estimate(&mut self, origin: ImmutableOrigin) -> Result<(u64, u64), String> {
        let result = write_or_roll_back(&self.store, Commit::Soon, |txn| {
            // <https://storage.spec.whatwg.org/#dom-storagemanager-estimate>
            // Let shelf be the result of running obtain a local storage shelf with this’s relevant
            // settings object.
            let shelf = obtain_a_local_storage_shelf(&origin, txn)?;

            // Let usage be storage usage for shelf.
            let paths = database_paths_of_bucket(shelf.default_bucket_id, txn)?;
            // Let quota be storage quota for shelf.
            let quota = storage_quota_for_bucket(shelf.default_bucket_id, txn)?;
            Ok::<_, String>((paths, quota))
        });

        let (paths, quota) = match result {
            Ok(result) => result?,
            Err(error) => return Err(error.to_string()),
        };
        // The walk of the directories, once the registry's transaction is over (see
        // `database_paths_of_bucket`).
        let usage = storage_usage(paths)?;
        Ok((usage, quota))
    }
}

pub trait ClientStorageThreadFactory {
    fn new(config_dir: Option<PathBuf>, temporary_storage: bool) -> Self;
}

impl ClientStorageThreadFactory for ClientStorageThreadHandle {
    fn new(config_dir: Option<PathBuf>, temporary_storage: bool) -> ClientStorageThreadHandle {
        let (generic_sender, generic_receiver) = generic_channel::channel().unwrap();
        let mut temp_dir: Option<tempfile::TempDir> = None;
        let base_dir = config_dir
            .unwrap_or_else(|| {
                let tmp_dir = tempfile::tempdir().unwrap();
                let path = tmp_dir.path().to_path_buf();
                temp_dir = Some(tmp_dir);
                path
            })
            .join("clientstorage");
        let storage_dir = if temporary_storage {
            let unique_id = uuid::Uuid::new_v4().to_string();
            base_dir.join("temporary").join(unique_id)
        } else {
            base_dir.join("default_v1")
        };
        std::fs::create_dir_all(&storage_dir)
            .expect("Failed to create ClientStorage storage directory");
        let sender_clone = generic_sender.clone();
        thread::Builder::new()
            .name("ClientStorageThread".to_owned())
            .spawn(move || {
                // Keep temp_dir alive while the thread runs.
                let _ = temp_dir;
                let engine = RedbEngine::new(storage_dir).unwrap_or_else(|error| {
                    warn!("Failed to initialize ClientStorage engine into storage dir: {error:?}");
                    RedbEngine::memory().unwrap()
                });
                ClientStorageThread::new(sender_clone, generic_receiver, engine).start();
            })
            .expect("Thread spawning failed");

        ClientStorageThreadHandle::new(generic_sender)
    }
}

struct ClientStorageThread<E: RegistryEngine> {
    receiver: GenericReceiver<ClientStorageThreadMessage>,
    sender: GenericSender<ClientStorageThreadMessage>,
    engine: E,
}

impl<E> ClientStorageThread<E>
where
    E: RegistryEngine,
{
    pub fn new(
        sender: GenericSender<ClientStorageThreadMessage>,
        receiver: GenericReceiver<ClientStorageThreadMessage>,
        engine: E,
    ) -> ClientStorageThread<E> {
        ClientStorageThread {
            sender,
            receiver,
            engine,
        }
    }

    pub fn start(mut self) {
        let mut exit_sender = None;
        while let Ok(message) = self.receiver.recv() {
            match message {
                ClientStorageThreadMessage::ObtainBottleMap {
                    storage_type,
                    storage_identifier,
                    webview,
                    origin,
                    sender,
                } => {
                    let result = self.engine.obtain_a_storage_bottle_map(
                        storage_type,
                        webview,
                        storage_identifier,
                        origin,
                        &self.sender,
                    );
                    let _ = sender.send(result.map_err(|e| format!("{:?}", e)));
                },
                ClientStorageThreadMessage::CreateDatabase {
                    bottle_id,
                    name,
                    sender,
                } => {
                    let result = self.engine.create_database(bottle_id, name);
                    let _ = sender.send(result.map_err(|e| format!("{:?}", e)));
                },
                ClientStorageThreadMessage::DeleteDatabase {
                    bottle_id,
                    name,
                    sender,
                } => {
                    let result = self.engine.delete_database(bottle_id, name);
                    let _ = sender.send(result.map_err(|e| format!("{:?}", e)));
                },
                ClientStorageThreadMessage::Persisted { origin, sender } => {
                    let _ = sender.send(self.engine.persisted(origin));
                },
                ClientStorageThreadMessage::Persist {
                    origin,
                    permission_granted,
                    sender,
                } => {
                    let _ = sender.send(self.engine.persist(origin, permission_granted));
                },
                ClientStorageThreadMessage::Estimate { origin, sender } => {
                    let _ = sender.send(self.engine.estimate(origin));
                },
                ClientStorageThreadMessage::Exit(sender) => {
                    exit_sender = Some(sender);
                    break;
                },
            }
        }
        // LucinaNet: close the registry before the ack, so that it is closed once the thread is
        // known to be gone (a redb file may be open once per process; Servo dropped its SQLite
        // connection after the ack).
        drop(self);
        if let Some(sender) = exit_sender {
            let _ = sender.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use profile_store::StoreError;
    use profile_store::redb::{ReadableTableMetadata, TableDefinition};
    use servo_base::generic_channel::{self, GenericSender};
    use servo_url::{ImmutableOrigin, ServoUrl};
    use storage_traits::client_storage::{
        ClientStorageErrorr, ClientStorageThreadMessage, StorageIdentifier, StorageType,
    };

    use super::{DATABASES, DIRECTORIES, RedbEngine, RegistryEngine, RegistryError};

    fn origin() -> ImmutableOrigin {
        ServoUrl::parse("https://example.com").unwrap().origin()
    }

    fn sender() -> GenericSender<ClientStorageThreadMessage> {
        generic_channel::channel().unwrap().0
    }

    fn bottle(engine: &mut RedbEngine) -> i64 {
        engine
            .obtain_a_storage_bottle_map(
                StorageType::Local,
                None,
                StorageIdentifier::IndexedDB,
                origin(),
                &sender(),
            )
            .unwrap()
            .bottle_id
    }

    fn rows(engine: &RedbEngine, table: TableDefinition<u64, &[u8]>) -> u64 {
        let txn = engine.store.read().unwrap();
        txn.open_table(table).unwrap().len().unwrap()
    }

    #[test]
    fn test_registry_survives_a_restart() {
        let base = tempfile::tempdir().unwrap();
        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        let bottle_id = bottle(&mut engine);
        let (kept, created) = engine
            .create_database(bottle_id, "kept".to_owned())
            .unwrap();
        assert!(created);
        let (gone, created) = engine
            .create_database(bottle_id, "gone".to_owned())
            .unwrap();
        assert!(created);
        assert!(engine.persist(origin(), true).unwrap());
        drop(engine);

        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        assert_eq!(bottle(&mut engine), bottle_id);
        assert!(engine.persisted(origin()).unwrap());
        assert_eq!(
            engine
                .create_database(bottle_id, "kept".to_owned())
                .unwrap(),
            (kept.clone(), false)
        );
        assert!(kept.is_dir());
        engine
            .delete_database(bottle_id, "gone".to_owned())
            .unwrap();
        assert!(!gone.exists());
        assert!(matches!(
            engine.delete_database(bottle_id, "gone".to_owned()),
            Err(ClientStorageErrorr::Internal(RegistryError::NoRow(_)))
        ));
        assert_eq!(rows(&engine, DATABASES), 1);
        assert_eq!(rows(&engine, DIRECTORIES), 1);
        drop(engine);

        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        let (again, created) = engine
            .create_database(bottle_id, "gone".to_owned())
            .unwrap();
        assert!(created);
        assert_ne!(again, gone);
        // A bottle that does not exist is refused, as the FOREIGN KEY did.
        assert!(matches!(
            engine.create_database(bottle_id + 100, "x".to_owned()),
            Err(ClientStorageErrorr::Internal(RegistryError::ForeignKey(_)))
        ));
    }

    #[test]
    fn test_orphan_directories_are_removed_at_startup() {
        let base = tempfile::tempdir().unwrap();
        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        let bottle_id = bottle(&mut engine);
        let (kept, _) = engine
            .create_database(bottle_id, "kept".to_owned())
            .unwrap();
        fs::write(kept.join("data"), b"kept").unwrap();
        drop(engine);

        // What a crash between creating a directory and committing its row leaves.
        let cluster = base.path().join("bottles").join("7");
        let orphan = cluster.join("00000000-0000-0000-0000-000000000007");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("indexeddb.redb"), b"orphan").unwrap();
        let empty_orphan = cluster.join("00000000-0000-0000-0000-000000000017");
        fs::create_dir_all(&empty_orphan).unwrap();
        // A directory without a row that holds more than an IndexedDB database: kept.
        let foreign = cluster.join("00000000-0000-0000-0000-000000000027");
        fs::create_dir_all(foreign.join("inner")).unwrap();
        fs::write(foreign.join("inner").join("data"), b"not ours").unwrap();
        let other_file = cluster.join("00000000-0000-0000-0000-000000000037");
        fs::create_dir_all(&other_file).unwrap();
        fs::write(other_file.join("indexeddb.redb"), b"orphan").unwrap();
        fs::write(other_file.join("notes.txt"), b"not ours").unwrap();
        // Files are not database directories: they stay.
        fs::write(cluster.join("note"), b"file").unwrap();

        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        assert!(!orphan.exists());
        assert!(!empty_orphan.exists());
        assert_eq!(
            fs::read(foreign.join("inner").join("data")).unwrap(),
            b"not ours"
        );
        assert!(other_file.join("notes.txt").is_file());
        assert!(other_file.join("indexeddb.redb").is_file());
        assert!(cluster.join("note").is_file());
        assert_eq!(fs::read(kept.join("data")).unwrap(), b"kept");
        assert_eq!(
            engine
                .create_database(bottle_id, "kept".to_owned())
                .unwrap(),
            (kept, false)
        );
    }

    #[test]
    fn test_a_new_registry_keeps_existing_directories() {
        // A profile whose registry this engine does not know (Servo's `reg.sqlite`, or a lost
        // `reg.redb`): its database directories have no row, and are not orphans of a crash.
        let base = tempfile::tempdir().unwrap();
        fs::write(base.path().join("reg.sqlite"), b"servo's registry").unwrap();
        let old = base
            .path()
            .join("bottles")
            .join("3")
            .join("00000000-0000-0000-0000-000000000003");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("indexeddb.sqlite"), b"an older database").unwrap();
        let only_redb = base
            .path()
            .join("bottles")
            .join("4")
            .join("00000000-0000-0000-0000-000000000004");
        fs::create_dir_all(&only_redb).unwrap();
        fs::write(only_redb.join("indexeddb.redb"), b"a database").unwrap();

        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        assert!(old.join("indexeddb.sqlite").is_file());
        assert!(only_redb.join("indexeddb.redb").is_file());
        // The registry works as usual next to them.
        let bottle_id = bottle(&mut engine);
        let (path, created) = engine.create_database(bottle_id, "db".to_owned()).unwrap();
        assert!(created);
        assert!(path.is_dir());
        drop(engine);

        // Once the registry exists, a directory with only `indexeddb.redb` and no row is an
        // orphan; one holding anything else is still kept.
        let engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        assert!(!only_redb.exists());
        assert!(old.join("indexeddb.sqlite").is_file());
        assert!(path.is_dir());
        let sweep = super::remove_orphan_directories(&engine.store, base.path(), true).unwrap();
        assert_eq!(
            sweep,
            super::Sweep {
                removed: 0,
                kept: 1
            }
        );
        let sweep = super::remove_orphan_directories(&engine.store, base.path(), false).unwrap();
        assert_eq!(
            sweep,
            super::Sweep {
                removed: 0,
                kept: 1
            }
        );
    }

    #[test]
    fn test_no_row_for_a_directory_that_could_not_be_created() {
        let base = tempfile::tempdir().unwrap();
        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        let bottle_id = bottle(&mut engine);

        // `bottles` is a file: no directory can be created under it.
        let bottles = base.path().join("bottles");
        fs::write(&bottles, b"in the way").unwrap();
        assert!(matches!(
            engine.create_database(bottle_id, "db".to_owned()),
            Err(ClientStorageErrorr::DirectoryCreationFailed)
        ));
        // Servo had committed both rows by then; here nothing was.
        assert_eq!(rows(&engine, DATABASES), 0);
        assert_eq!(rows(&engine, DIRECTORIES), 0);

        fs::remove_file(&bottles).unwrap();
        let (path, created) = engine.create_database(bottle_id, "db".to_owned()).unwrap();
        assert!(created);
        assert!(path.is_dir());
        assert_eq!(rows(&engine, DIRECTORIES), 1);
        // Every directory a row refers to exists.
        let txn = engine.store.read().unwrap();
        let directories = txn.open_table(DIRECTORIES).unwrap();
        for row in profile_store::redb::ReadableTable::iter(&directories).unwrap() {
            let (_, record) = row.unwrap();
            let (_, path) = super::read_pair(record.value()).unwrap();
            assert!(Path::new(&path).is_dir());
        }
    }

    #[test]
    fn test_corrupt_rows_are_errors() {
        let base = tempfile::tempdir().unwrap();
        let mut engine = RedbEngine::new(base.path().to_path_buf()).unwrap();
        let bottle_id = bottle(&mut engine);
        engine.create_database(bottle_id, "db".to_owned()).unwrap();
        engine
            .store
            .write(|txn| {
                let mut directories = txn.open_table(DIRECTORIES)?;
                directories.insert(1, &[1, 2, 3][..])?;
                Ok(())
            })
            .unwrap();
        assert!(matches!(
            engine.create_database(bottle_id, "db".to_owned()),
            Err(ClientStorageErrorr::Internal(RegistryError::Store(
                StoreError::Corrupt(_)
            )))
        ));
        assert!(matches!(
            engine.delete_database(bottle_id, "db".to_owned()),
            Err(ClientStorageErrorr::Internal(RegistryError::Store(
                StoreError::Corrupt(_)
            )))
        ));
        assert!(engine.estimate(origin()).is_err());

        // A bucket that does not decode: "false when there's an internal error".
        engine
            .store
            .write(|txn| {
                let mut buckets = txn.open_table(super::BUCKETS)?;
                buckets.insert(1, &[9][..])?;
                Ok(())
            })
            .unwrap();
        assert!(!engine.persisted(origin()).unwrap());
        assert!(!engine.persist(origin(), true).unwrap());
    }
}
