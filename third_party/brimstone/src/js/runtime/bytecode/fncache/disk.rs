//! On-disk function cache store: one file per key in a directory, written behind a background
//! writer thread, bounded in size by LRU eviction.
//!
//! File format: magic (4 bytes) + format version (u32 LE) + engine build id (u64 LE) +
//! payload length (u32 LE) + payload. Every read is bounds-checked; corrupt or truncated files
//! are ignored, never a panic.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use super::disk_rules::{self, Lru};
use super::template::{self, Template};
use super::wire;

pub const MAGIC: &[u8; 4] = b"FNC1";
// Bumped whenever opcode numbering changes: the build id only covers the crate version, so a new
// instruction in the middle of the list would otherwise let an old cache decode as wrong opcodes.
pub const FORMAT_VERSION: u32 = 5;

/// Default bound on the total size of the on-disk store.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The engine build id: a hash of the crate version, so caches from different engine builds are
/// never mixed.
pub fn build_id() -> u64 {
    // A hash of every source file of the engine (build.rs): opcodes and layouts change without the package version changing.
    u64::from_str_radix(env!("LUCINAJS_BUILD_ID"), 16).expect("build.rs writes 16 hex digits")
}

pub fn key_file_name(key: u128) -> String {
    format!("{key:032x}.fnc")
}

fn key_from_file_name(name: &str) -> Option<u128> {
    let hex = name.strip_suffix(".fnc")?;
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(hex, 16).ok()
}

/// Number of bytes in a record header: magic (4) + format version (4) + build id (8) + key (16) +
/// partition digest (16) + payload length (4).
pub const HEADER_LEN: usize = wire::HEADER_LEN;

/// Serialize a full record (header + payload).
pub fn serialize_record(key: u128, partition: u128, template: &Template) -> Vec<u8> {
    let payload = template::serialize(template);
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    wire::write_header(
        &mut buf,
        MAGIC,
        FORMAT_VERSION,
        build_id(),
        key,
        partition,
        payload.len() as u32,
    );
    buf.extend_from_slice(&payload);
    buf
}

/// Parse a full record. Returns None if the header does not match or the payload is corrupt.
pub fn deserialize_record(buf: &[u8], key: u128, partition: u128) -> Option<Template> {
    // The magic, the format version, the build id and the payload length are all checked, and the key and the partition digest
    // are the ones in the header (wire::read_header, Verus); the hit decision is partition::may_hit (Verus).
    let (entry_key, entry_partition) = wire::read_header(buf, MAGIC, FORMAT_VERSION, build_id())?;
    if !super::partition::may_hit(entry_key, entry_partition, key, partition) {
        return None;
    }
    template::deserialize(&buf[HEADER_LEN..])
}

enum WriterMsg {
    Store(u128, Vec<u8>),
    Touch(u128),
    Flush(Sender<()>),
    Shutdown,
}

/// Make the directory entries of `dir` (created, renamed or unlinked files) durable. A file that was
/// synced but whose directory was not can vanish entirely on a power cut (ext4).
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

struct WriterState {
    dir: PathBuf,
    /// Stores and evictions that failed (write, fsync, rename or directory fsync), shared with the `DiskStore`.
    failed: Arc<AtomicU64>,
    /// The records on disk and their bytes; every decision about the size bound is `disk_rules`'.
    lru: Lru,
}

impl WriterState {
    fn touch(&mut self, key: u128) {
        self.lru.touch(key);
    }

    /// Unlink the records the rules evicted, then fsync the directory once.
    fn unlink(&self, keys: &[u128]) {
        if keys.is_empty() {
            return;
        }
        for key in keys {
            let _ = fs::remove_file(self.dir.join(key_file_name(*key)));
        }
        if sync_dir(&self.dir).is_err() {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Write the record to a temporary file, fsync it, rename it over the final name and fsync the
    /// directory. Any failure removes what was written and leaves the store as it was.
    fn write_durably(&self, key: u128, record: &[u8]) -> std::io::Result<()> {
        let path = self.dir.join(key_file_name(key));
        let tmp_path = self.dir.join(format!("{key:032x}.tmp"));
        let result = (|| {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(record)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp_path, &path)?;
            sync_dir(&self.dir)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    }

    fn store(&mut self, key: u128, record: Vec<u8>) {
        let size = record.len() as u64;
        // A record larger than the bound is never written.
        if !disk_rules::accepts(size, self.lru.max) {
            return;
        }
        if self.write_durably(key, &record).is_err() {
            self.failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let evicted = self.lru.insert(key, size);
        self.unlink(&evicted);
    }
}

fn writer_thread(mut state: WriterState, rx: Receiver<WriterMsg>) {
    while let Ok(msg) = rx.recv() {
        match msg {
            WriterMsg::Store(key, record) => state.store(key, record),
            WriterMsg::Touch(key) => state.touch(key),
            WriterMsg::Flush(ack) => {
                let _ = ack.send(());
            }
            WriterMsg::Shutdown => break,
        }
    }
}

pub struct DiskStore {
    dir: PathBuf,
    max_bytes: u64,
    /// Lazily loaded index: key -> (path, size).
    index: Option<HashMap<u128, PathBuf>>,
    sender: Sender<WriterMsg>,
    thread: Option<JoinHandle<()>>,
    failed: Arc<AtomicU64>,
}

impl DiskStore {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        let (sender, rx) = mpsc::channel();
        // Build the initial LRU state from the existing files, ordered by modification time.
        let mut entries: Vec<(u128, u64, std::time::SystemTime)> = Vec::new();
        if let Ok(read_dir) = fs::read_dir(&dir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Some(key) = key_from_file_name(name) else {
                    continue;
                };
                if let Ok(meta) = entry.metadata() {
                    let size = meta.len();
                    let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    entries.push((key, size, mtime));
                }
            }
        }
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        // Two spellings of one key (upper and lower case hex) are one record to the rules: the first is kept.
        let mut seen = HashSet::new();
        let existing: Vec<(u128, u64)> = entries
            .into_iter()
            .filter(|(key, _, _)| seen.insert(*key))
            .map(|(key, size, _)| (key, size))
            .collect();
        let failed = Arc::new(AtomicU64::new(0));
        let (lru, evicted) = Lru::from_existing(&existing, max_bytes);
        let state = WriterState { dir: dir.clone(), failed: Arc::clone(&failed), lru };
        state.unlink(&evicted);
        let thread = std::thread::Builder::new()
            .name("fncache-writer".to_string())
            .spawn(move || writer_thread(state, rx))
            .ok()?;
        Some(DiskStore {
            dir,
            max_bytes,
            index: None,
            sender,
            thread: Some(thread),
            failed,
        })
    }

    fn load_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        let mut index = HashMap::new();
        if let Ok(read_dir) = fs::read_dir(&self.dir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if let Some(key) = key_from_file_name(name) {
                    index.insert(key, entry.path());
                }
            }
        }
        self.index = Some(index);
    }

    /// Read a template from disk. Reads at most one file; corrupt files are ignored.
    pub fn load(&mut self, key: u128, partition: u128) -> Option<Template> {
        self.load_index();
        let path = self.index.as_ref()?.get(&key)?.clone();
        let buf = fs::read(&path).ok()?;
        let template = deserialize_record(&buf, key, partition)?;
        // Record the use for LRU purposes (asynchronous, never blocks on I/O).
        let _ = self.sender.send(WriterMsg::Touch(key));
        Some(template)
    }

    /// Whether a key is present on disk (without reading the file).
    #[allow(dead_code)]
    pub fn contains(&mut self, key: u128) -> bool {
        self.load_index();
        self.index.as_ref().is_some_and(|i| i.contains_key(&key))
    }

    /// Queue a record to be written behind the writer thread.
    pub fn store(&self, key: u128, partition: u128, template: &Template) {
        let record = serialize_record(key, partition, template);
        let _ = self.sender.send(WriterMsg::Store(key, record));
    }

    /// Block until all queued writes have been performed. Used by tests and tools.
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.sender.send(WriterMsg::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }

    /// Number of stores and evictions that failed so far (including failed fsyncs). A failed store
    /// leaves no record behind; the in-memory cache is unaffected.
    pub fn failed_stores(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

impl Drop for DiskStore {
    fn drop(&mut self) {
        let _ = self.sender.send(WriterMsg::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    const PARTITION: u128 = 5;
    use super::*;
    use crate::runtime::bytecode::constant_table_builder::Prov;
    use crate::runtime::bytecode::fncache::key::{FuncKind, NameProv};
    use crate::runtime::bytecode::fncache::template::{
        ConstEntry, HandlerEntry, HeapKind, NestedEntry, SourceMapTemplate, Template,
    };

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fncache-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sample_template(seed: u8) -> Template {
        Template {
            bytecode: vec![seed; 64],
            source_map: SourceMapTemplate {
                range_node: 0,
                range_start_delta: 0,
                range_end_delta: 10,
                entries: vec![(0, 0, 0), (8, 1, 4)],
            },
            constants: vec![
                ConstEntry::String { prov: Prov::Intrinsic, text: Some(b"x".to_vec()) },
                ConstEntry::HeapItem {
                    kind: HeapKind::NestedFunction,
                    prov: Prov::NestedFunction(0),
                    class_names: None,
                    scope_names: None,
                },
                ConstEntry::Double(1.0f64.to_bits()),
            ],
            handlers: vec![HandlerEntry { start: 0, end: 8, handler: 16, error_register: -1 }],
            handler_width: 0,
            num_caches: 1,
            num_registers: 4,
            num_parameters: 1,
            function_length: 1,
            estimated_num_properties: 0,
            flags: 0,
            new_target_index: None,
            name_prov: NameProv::None,
            name_text: None,
            nested: vec![NestedEntry {
                constant_index: 1,
                node_ordinal: 2,
                kind: FuncKind::Declaration,
                name_prov: NameProv::None,
                name_text: None,
                is_base: false,
                class_ordinal: None,
                scope_chain: vec![],
            }],
            patches: vec![],
            size: 256,
        }
    }

    #[test]
    fn disk_store_round_trip_across_stores() {
        let dir = temp_dir("roundtrip");
        let key: u128 = 0x1234_5678_9abc_def0_1111_2222_3333_4444;
        let template = sample_template(1);

        {
            let store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
            store.store(key, PARTITION, &template);
            store.flush();
        }

        // A second store over the same directory, like a second process.
        let mut store2 = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
        let loaded = store2
            .load(key, PARTITION)
            .expect("template must load from disk");
        assert_eq!(loaded, template);
        drop(store2);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_and_truncated_files_are_ignored() {
        let dir = temp_dir("corrupt");
        let key: u128 = 0xdead_beef_dead_beef_dead_beef_dead_beef;
        let template = sample_template(2);

        {
            let store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
            store.store(key, PARTITION, &template);
            store.flush();
        }

        let path = dir.join(key_file_name(key));

        // Corrupt one payload byte: the record must be rejected, never panic.
        let mut buf = fs::read(&path).unwrap();
        buf[HEADER_LEN] ^= 0xff;
        fs::write(&path, &buf).unwrap();
        let mut store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
        assert!(store.load(key, PARTITION).is_none(), "corrupt record must be ignored");
        drop(store);

        // Truncate the file: must be ignored as well.
        let buf = fs::read(&path).unwrap();
        fs::write(&path, &buf[..buf.len() / 2]).unwrap();
        let mut store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
        assert!(store.load(key, PARTITION).is_none(), "truncated record must be ignored");
        drop(store);

        // Wrong magic.
        let mut buf = fs::read(&path).unwrap();
        buf[0] = b'X';
        fs::write(&path, &buf).unwrap();
        let mut store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
        assert!(store.load(key, PARTITION).is_none());
        drop(store);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_store_is_counted_and_leaves_nothing() {
        let dir = temp_dir("failed");
        let store = DiskStore::new(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
        store.store(1, PARTITION, &sample_template(1));
        store.flush();
        assert_eq!(store.failed_stores(), 0);
        // The directory disappears under the store: the next store fails without a panic.
        fs::remove_dir_all(&dir).unwrap();
        store.store(2, PARTITION, &sample_template(2));
        store.flush();
        assert_eq!(store.failed_stores(), 1);
        drop(store);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn record_round_trips_and_other_versions_are_refused() {
        let template = sample_template(5);
        let record = serialize_record(1, PARTITION, &template);
        assert_eq!(deserialize_record(&record, 1, PARTITION).as_ref(), Some(&template));
        // Another format version, another build id, another magic, a payload length that is not the rest: all refused.
        for (at, len) in [
            (0usize, 4usize),
            (4, 4),
            (8, 8),
            (16, 16),
            (32, 16),
            (48, 4),
        ] {
            for i in at..at + len {
                let mut other = record.clone();
                other[i] ^= 1;
                assert!(
                    deserialize_record(&other, 1, PARTITION).is_none(),
                    "byte {i} of the header changed but the record was accepted"
                );
            }
        }
        // The key and the partition asked for must be the ones in the header.
        assert!(deserialize_record(&record, 1, PARTITION + 1).is_none());
        assert!(deserialize_record(&record, 2, PARTITION).is_none());
        let mut longer = record.clone();
        longer.push(0);
        assert!(deserialize_record(&longer, 1, PARTITION).is_none());
        for cut in 0..record.len() {
            assert!(deserialize_record(&record[..cut], 1, PARTITION).is_none());
        }
    }

    #[test]
    fn oversized_record_is_never_written() {
        let dir = temp_dir("oversized");
        let record_len = serialize_record(1, PARTITION, &sample_template(1)).len() as u64;
        let store = DiskStore::new(dir.clone(), record_len - 1).unwrap();
        store.store(1, PARTITION, &sample_template(1));
        store.flush();
        let files = fs::read_dir(&dir).unwrap().count();
        assert_eq!(files, 0, "a record larger than the bound must not touch the disk");
        assert_eq!(store.failed_stores(), 0);
        drop(store);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn least_recently_used_record_goes_first() {
        let dir = temp_dir("lru-order");
        let record_len = serialize_record(1, PARTITION, &sample_template(1)).len() as u64;
        let mut store = DiskStore::new(dir.clone(), 2 * record_len + 1).unwrap();
        store.store(1, PARTITION, &sample_template(1));
        store.store(2, PARTITION, &sample_template(2));
        store.flush();
        // Using record 1 makes record 2 the least recently used.
        assert!(store.load(1, PARTITION).is_some());
        store.flush();
        store.store(3, PARTITION, &sample_template(3));
        store.flush();
        assert!(dir.join(key_file_name(1)).exists());
        assert!(!dir.join(key_file_name(2)).exists());
        assert!(dir.join(key_file_name(3)).exists());
        drop(store);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lru_bound_is_enforced() {
        let dir = temp_dir("lru");
        let max_bytes: u64 = 2048;
        let num = 12u128;

        {
            let store = DiskStore::new(dir.clone(), max_bytes).unwrap();
            for i in 0..num {
                store.store(i, PARTITION, &sample_template(i as u8));
                store.flush();
            }
        }

        let total: u64 = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(total <= max_bytes, "on-disk size {total} exceeds bound {max_bytes}");

        // The most recently stored key must still be present.
        let mut store = DiskStore::new(dir.clone(), max_bytes).unwrap();
        assert!(
            store.load(num - 1, PARTITION).is_some(),
            "most recent key must survive eviction"
        );
        drop(store);

        fs::remove_dir_all(&dir).ok();
    }
}
