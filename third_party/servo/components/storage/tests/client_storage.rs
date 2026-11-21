/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::PathBuf;

use profile_store::Store;
use profile_store::record::Reader;
use profile_store::redb::{ReadTransaction, ReadableTable, ReadableTableMetadata, TableDefinition};
use servo_base::generic_channel;
use servo_base::generic_channel::GenericCallback;
use servo_base::id::{
    BrowsingContextId, PIPELINE_NAMESPACE, PipelineNamespace, PipelineNamespaceId, WebViewId,
};
use servo_url::ServoUrl;
use storage::ClientStorageThreadFactory;
use storage_traits::client_storage::{
    ClientStorageThreadHandle, ClientStorageThreadMessage, StorageIdentifier, StorageProxyMap,
    StorageType,
};

fn install_test_namespace() {
    if PIPELINE_NAMESPACE.get().is_none() {
        PipelineNamespace::install(PipelineNamespaceId(1));
    }
}

fn registry_db_path(tmp_dir: &tempfile::TempDir) -> PathBuf {
    tmp_dir
        .path()
        .join("clientstorage")
        .join("default_v1")
        .join("reg.redb")
}

// The registry's tables, as components/storage/client_storage.rs defines them.
const SHEDS: TableDefinition<u64, &[u8]> = TableDefinition::new("sheds");
const SHELVES: TableDefinition<u64, &[u8]> = TableDefinition::new("shelves");
const BUCKETS: TableDefinition<u64, &[u8]> = TableDefinition::new("buckets");
const BOTTLES: TableDefinition<u64, &[u8]> = TableDefinition::new("bottles");

/// Opens the registry once its thread has closed it: a redb file may be open once per process,
/// where Servo's test opened a second SQLite connection next to the thread's.
fn open_registry(handle: &ClientStorageThreadHandle, tmp_dir: &tempfile::TempDir) -> Store {
    let (sender, receiver) = generic_channel::channel().unwrap();
    handle
        .send(ClientStorageThreadMessage::Exit(sender))
        .unwrap();
    receiver.recv().unwrap();
    let path = registry_db_path(tmp_dir);
    for _ in 0..300 {
        if let Ok(store) = Store::open(&path) {
            return store;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the registry was not closed");
}

/// `SELECT COUNT(*) FROM sheds WHERE storage_type = ?1 AND browsing_context IS ?2;`
fn count_sheds(txn: &ReadTransaction, storage_type: &str, browsing_context: Option<&str>) -> i64 {
    let sheds = txn.open_table(SHEDS).unwrap();
    let mut count = 0;
    for row in sheds.iter().unwrap() {
        let (_, record) = row.unwrap();
        let mut reader = Reader::new(record.value());
        let row_type = reader.string().unwrap();
        let row_context = reader.opt_string().unwrap();
        reader.end().unwrap();
        if row_type == storage_type && row_context.as_deref() == browsing_context {
            count += 1;
        }
    }
    count
}

/// `SELECT COUNT(*) FROM shelves WHERE origin = ?1;`
fn count_shelves(txn: &ReadTransaction, origin: &str) -> i64 {
    let shelves = txn.open_table(SHELVES).unwrap();
    let mut count = 0;
    for row in shelves.iter().unwrap() {
        let (_, record) = row.unwrap();
        let mut reader = Reader::new(record.value());
        let _shed_id = reader.u64().unwrap();
        let row_origin = reader.string().unwrap();
        reader.end().unwrap();
        if row_origin == origin {
            count += 1;
        }
    }
    count
}

/// `SELECT COUNT(*) FROM <table>;`
fn count_rows(txn: &ReadTransaction, table: TableDefinition<u64, &[u8]>) -> i64 {
    txn.open_table(table).unwrap().len().unwrap() as i64
}

fn obtain_bottle_map(
    handle: &ClientStorageThreadHandle,
    storage_type: StorageType,
    webview: Option<WebViewId>,
    storage_identifier: StorageIdentifier,
    origin: servo_url::ImmutableOrigin,
) -> StorageProxyMap {
    handle
        .obtain_a_storage_bottle_map(storage_type, webview, storage_identifier, origin)
        .recv()
        .unwrap()
        .unwrap()
}

#[test]
fn test_exit() {
    let handle: ClientStorageThreadHandle = ClientStorageThreadFactory::new(None, false);

    let (sender, receiver) = generic_channel::channel().unwrap();
    handle
        .send(ClientStorageThreadMessage::Exit(sender))
        .unwrap();
    receiver.recv().unwrap();

    // Workaround for https://github.com/servo/servo/issues/32912
    #[cfg(windows)]
    std::thread::sleep(std::time::Duration::from_millis(1000));
}

#[test]
fn test_workflow() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);

    let url = ServoUrl::parse("https://example.com").unwrap();

    // Obtain a first storage proxy map.
    let storage_proxy_map = handle
        .obtain_a_storage_bottle_map(
            StorageType::Local,
            Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID)),
            StorageIdentifier::IndexedDB,
            url.origin(),
        )
        .recv()
        .unwrap()
        .unwrap();

    // Create a db.
    let receiver = handle.create_database(storage_proxy_map.bottle_id, "test1".to_string());
    let (path, created) = receiver.recv().unwrap().expect("Path should be created");
    assert!(created);

    assert!(std::fs::read_dir(path.clone()).is_ok());

    // Create another db with the same name.
    let receiver = handle.create_database(storage_proxy_map.bottle_id, "test1".to_string());
    let (path, created) = receiver.recv().unwrap().expect("Path should be created");
    assert!(!created);

    // Create another db with a different same.
    let receiver = handle.create_database(storage_proxy_map.bottle_id, "test2".to_string());
    let (yet_another_path, created) = receiver.recv().unwrap().expect("Path should be created");
    assert_ne!(path, yet_another_path);
    assert!(created);

    // Delete the dbs.
    let receiver = handle.delete_database(storage_proxy_map.bottle_id, "test1".to_string());
    receiver.recv().unwrap().expect("Db should be deleted");
    let receiver = handle.delete_database(storage_proxy_map.bottle_id, "test2".to_string());
    receiver.recv().unwrap().expect("Db should be deleted");

    assert!(std::fs::read_dir(path).is_err());

    // Get another proxy map fro the same parameters.
    let second_proxy_map = handle
        .obtain_a_storage_bottle_map(
            StorageType::Local,
            Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID)),
            StorageIdentifier::IndexedDB,
            url.origin(),
        )
        .recv()
        .unwrap()
        .unwrap();

    // Bottle id should be the same, because the manager returned the existing id.
    assert_eq!(second_proxy_map.bottle_id, storage_proxy_map.bottle_id);

    // Workaround for https://github.com/servo/servo/issues/32912
    #[cfg(windows)]
    std::thread::sleep(std::time::Duration::from_millis(1000));
}

#[test]
fn test_repeated_local_obtain_reuses_same_logical_rows() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);

    let origin = ServoUrl::parse("https://example.com").unwrap().origin();
    let webview = Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID));

    let first = obtain_bottle_map(
        &handle,
        StorageType::Local,
        webview,
        StorageIdentifier::IndexedDB,
        origin.clone(),
    );
    let second = obtain_bottle_map(
        &handle,
        StorageType::Local,
        webview,
        StorageIdentifier::IndexedDB,
        origin.clone(),
    );

    assert_eq!(first.bottle_id, second.bottle_id);

    let registry = open_registry(&handle, &tmp_dir);
    let txn = registry.read().unwrap();
    let local_shed_count = count_sheds(&txn, "local", None);
    let shelf_count = count_shelves(&txn, &origin.ascii_serialization());
    let bucket_count = count_rows(&txn, BUCKETS);
    let bottle_count = count_rows(&txn, BOTTLES);

    assert_eq!(local_shed_count, 1);
    assert_eq!(shelf_count, 1);
    assert_eq!(bucket_count, 1);
    assert_eq!(bottle_count, 4);
}

#[test]
fn test_repeated_session_obtain_reuses_same_logical_rows() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);

    let origin = ServoUrl::parse("https://example.com").unwrap().origin();
    let webview = Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID));
    let browsing_context = Into::<BrowsingContextId>::into(webview.clone().unwrap()).to_string();

    let first = obtain_bottle_map(
        &handle,
        StorageType::Session,
        webview,
        StorageIdentifier::SessionStorage,
        origin.clone(),
    );
    let second = obtain_bottle_map(
        &handle,
        StorageType::Session,
        webview,
        StorageIdentifier::SessionStorage,
        origin.clone(),
    );

    assert_eq!(first.bottle_id, second.bottle_id);

    let registry = open_registry(&handle, &tmp_dir);
    let txn = registry.read().unwrap();
    let session_shed_count = count_sheds(&txn, "session", Some(&browsing_context));
    let shelf_count = count_shelves(&txn, &origin.ascii_serialization());
    let bucket_count = count_rows(&txn, BUCKETS);
    let bottle_count = count_rows(&txn, BOTTLES);

    assert_eq!(session_shed_count, 1);
    assert_eq!(shelf_count, 1);
    assert_eq!(bucket_count, 1);
    assert_eq!(bottle_count, 1);
}

#[test]
fn test_local_persistence_and_estimate() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);

    let origin = ServoUrl::parse("https://example.com").unwrap().origin();
    let webview = WebViewId::new(servo_base::id::TEST_PAINTER_ID);
    let storage_proxy_map = obtain_bottle_map(
        &handle,
        StorageType::Local,
        Some(webview),
        StorageIdentifier::IndexedDB,
        origin.clone(),
    );

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persisted(origin.clone(), cb).unwrap();
    assert!(!rx.recv().unwrap().unwrap());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persist(origin.clone(), false, cb).unwrap();
    assert!(!rx.recv().unwrap().unwrap());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persisted(origin.clone(), cb).unwrap();
    assert!(!rx.recv().unwrap().unwrap());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persist(origin.clone(), true, cb).unwrap();
    assert!(rx.recv().unwrap().unwrap());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persisted(origin.clone(), cb).unwrap();
    assert!(rx.recv().unwrap().unwrap());

    let (path, _created) = handle
        .create_database(storage_proxy_map.bottle_id, "estimate".to_string())
        .recv()
        .unwrap()
        .unwrap();
    let payload = vec![0x5a; 8192];
    std::fs::write(path.join("payload.bin"), &payload).unwrap();

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.estimate(origin, cb).unwrap();
    let (usage, quota) = rx.recv().unwrap().unwrap();
    assert!(usage >= payload.len() as u64);
    assert!(quota > usage);
}

#[test]
fn test_storage_manager_operations_fail_for_opaque_origins() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);

    let origin = ServoUrl::parse("data:text/plain,hello").unwrap().origin();

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persisted(origin.clone(), cb).unwrap();
    assert!(rx.recv().unwrap().is_err());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.persist(origin.clone(), true, cb).unwrap();
    assert!(rx.recv().unwrap().is_err());

    let (cb, rx) = GenericCallback::new_blocking().unwrap();
    handle.estimate(origin, cb).unwrap();
    assert!(rx.recv().unwrap().is_err());
}

#[test]
fn test_registry_survives_a_thread_restart() {
    install_test_namespace();
    let tmp_dir = tempfile::tempdir().unwrap();
    let origin = ServoUrl::parse("https://example.com").unwrap().origin();
    let webview = Some(WebViewId::new(servo_base::id::TEST_PAINTER_ID));

    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);
    let first = obtain_bottle_map(
        &handle,
        StorageType::Local,
        webview,
        StorageIdentifier::IndexedDB,
        origin.clone(),
    );
    let (path, created) = handle
        .create_database(first.bottle_id, "kept".to_string())
        .recv()
        .unwrap()
        .unwrap();
    assert!(created);
    let (sender, receiver) = generic_channel::channel().unwrap();
    handle
        .send(ClientStorageThreadMessage::Exit(sender))
        .unwrap();
    receiver.recv().unwrap();

    let handle: ClientStorageThreadHandle =
        ClientStorageThreadFactory::new(Some(tmp_dir.path().to_path_buf()), false);
    let second = obtain_bottle_map(
        &handle,
        StorageType::Local,
        webview,
        StorageIdentifier::IndexedDB,
        origin,
    );
    assert_eq!(first.bottle_id, second.bottle_id);
    let (again, created) = handle
        .create_database(second.bottle_id, "kept".to_string())
        .recv()
        .unwrap()
        .unwrap();
    assert!(!created);
    assert_eq!(again, path);
    assert!(path.is_dir());
}
