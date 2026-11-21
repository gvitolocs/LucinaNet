//! Structural per-function bytecode cache.
//!
//! - `key`: the 128-bit structural key of a function (see there for the design).
//! - `template`: what is stored on a miss and instantiated on a hit.
//! - `disk`: the optional on-disk store.
//!
//! The cache itself lives on the `Context` (`ContextCell::fn_cache`) and is driven from
//! `BytecodeProgramGenerator::gen_function`.

pub mod disk;
// proof text: the blocks hold //@ proof lines; sites: src/js/runtime/bytecode/fncache/disk_rules.rs:346, 368
#[allow(clippy::single_match)]
pub mod disk_rules;
pub mod key;
pub mod partition;
// proof text: the &mut Vec<u8> signature is part of the proved statement; sites: src/js/runtime/bytecode/fncache/patch_rules.rs:39
#[allow(clippy::ptr_arg)]
pub mod patch_rules;
// proof text: two fncache mutants search the text; sites: src/js/runtime/bytecode/fncache/prov_rules.rs:77
#[allow(clippy::match_like_matches_macro)]
// proof text: three mutants search it; sites: src/js/runtime/bytecode/fncache/prov_rules.rs:100
#[allow(clippy::nonminimal_bool)]
pub mod prov_rules;
pub mod secret;
pub mod template;
// proof text: ensures stated per Some/None arm, body mirrors it; sites: src/js/runtime/bytecode/fncache/wire.rs:279, 293, 307, 321, 341
#[allow(clippy::manual_map)]
// proof text: the proof steps through the arms; sites: src/js/runtime/bytecode/fncache/wire.rs:337, 474, 481, 488, 492, 496, 500, 504
#[allow(clippy::question_mark)]
pub mod wire;

use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

pub use key::UncacheableReason;
use rustc_hash::FxHashMap as HashMap;
use template::Template;

/// Statistics of the function cache. All counters are cumulative for the context's lifetime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FunctionCacheStats {
    /// Number of cache lookups (functions whose key was computed and looked up).
    pub lookups: u64,
    /// Number of hits: a template was found and instantiated.
    pub hits: u64,
    /// Number of misses: no template for the key.
    pub misses: u64,
    /// Number of functions that could not be cached, by reason.
    pub uncacheable: [u64; UncacheableReason::COUNT],
    /// Number of verify-mode mismatches (a hit whose instantiation differed from generation).
    pub verify_mismatches: u64,
    /// Approximate number of bytes stored (in memory and queued for disk).
    pub bytes_stored: u64,
    /// Total time spent computing keys, in nanoseconds.
    pub key_time_ns: u64,
    /// Total time spent instantiating templates, in nanoseconds.
    pub instantiate_time_ns: u64,
}

/// Templates shared by all contexts of the process that opt in with `FunctionCache::shared`.
/// Templates are plain data, independent of any context, so contexts can reuse each other's.
static SHARED: LazyLock<Mutex<HashMap<u128, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));

/// A cached template with what it was stored under.
#[derive(Clone)]
struct Entry {
    key: u128,
    partition: u128,
    template: Arc<Template>,
}

/// The per-context function cache: an in-memory map plus an optional on-disk store.
pub struct FunctionCache {
    /// Whether the cache is enabled. When disabled, no keys are computed and nothing is stored.
    /// Off by default: the embedder or the options switch it on.
    pub enabled: bool,
    /// Whether templates are also shared with the other contexts of the process.
    pub shared: bool,
    /// Verify mode: on every hit, also generate the function normally and compare.
    pub verify: bool,
    /// The secret that keys the hash of function keys, see `secret`.
    pub secret: secret::KeySecret,
    mem: HashMap<u128, Entry>,
    /// Partitions (top-level sites) set on realms of this context, by id (index + 1), with the
    /// digest of each under the current secret. Id 0 is "no partition".
    partitions: Vec<(Box<[u8]>, u128)>,
    mem_bytes: usize,
    disk: Option<disk::DiskStore>,
    pub stats: FunctionCacheStats,
}

impl FunctionCache {
    pub fn new() -> Self {
        FunctionCache {
            enabled: false,
            shared: false,
            verify: false,
            secret: secret::process_secret(),
            mem: HashMap::default(),
            partitions: Vec::new(),
            mem_bytes: 0,
            disk: None,
            stats: FunctionCacheStats::default(),
        }
    }

    /// Set the on-disk store directory. `None` disables the disk store.
    ///
    /// The keys are made with the directory's own secret, so records of another secret (or of a
    /// directory whose secret file was lost) never hit. Templates already in memory were keyed
    /// with the previous secret and are dropped. A directory whose secret cannot be made
    /// durable is not used.
    pub fn set_dir(&mut self, dir: Option<PathBuf>) {
        self.disk = None;
        let Some(dir) = dir else { return };
        let Some(secret) = secret::load_or_create(&dir) else {
            return;
        };
        self.secret = secret;
        self.mem.clear();
        self.mem_bytes = 0;
        for (bytes, digest) in &mut self.partitions {
            *digest = digest_partition(&secret, bytes);
        }
        self.disk = disk::DiskStore::new(dir, disk::DEFAULT_MAX_BYTES);
    }

    /// Set the on-disk size bound in bytes.
    pub fn set_max_disk_bytes(&mut self, max_bytes: u64) {
        if let Some(disk) = &mut self.disk {
            // Recreate the store with the new bound; existing files are re-indexed.
            let dir = disk.dir().to_path_buf();
            self.disk = disk::DiskStore::new(dir, max_bytes);
        }
    }

    /// The id of a partition (a top-level site) for realms of this context. Equal bytes give the
    /// same id. An empty partition is "no partition": id 0, which gets no cache.
    pub fn partition_id(&mut self, partition: &[u8]) -> u32 {
        if partition.is_empty() {
            return 0;
        }
        if let Some(index) = self
            .partitions
            .iter()
            .position(|(bytes, _)| **bytes == *partition)
        {
            return index as u32 + 1;
        }
        let digest = digest_partition(&self.secret, partition);
        self.partitions.push((partition.into(), digest));
        self.partitions.len() as u32
    }

    /// The digest of a partition under the current secret, or None for "no partition".
    pub fn partition_digest(&self, id: u32) -> Option<u128> {
        let index = (id as usize).checked_sub(1)?;
        self.partitions.get(index).map(|(_, digest)| *digest)
    }

    /// Look up a key of a partition: first in memory, then on disk (reading at most one file).
    /// A record is used only if its key and its partition are both equal to the lookup's.
    pub fn lookup(&mut self, key: u128, partition: u128) -> Option<Arc<Template>> {
        if let Some(entry) = self.mem.get(&key) {
            if partition::may_hit(entry.key, entry.partition, key, partition) {
                return Some(Arc::clone(&entry.template));
            }
        }
        if self.shared {
            let found = SHARED.lock().unwrap().get(&key).cloned();
            if let Some(entry) = found {
                if partition::may_hit(entry.key, entry.partition, key, partition) {
                    self.mem_bytes += entry.template.size;
                    let template = Arc::clone(&entry.template);
                    self.mem.insert(key, entry);
                    return Some(template);
                }
            }
        }
        if let Some(disk) = &mut self.disk {
            if let Some(template) = disk.load(key, partition) {
                let template = Arc::new(template);
                self.mem_bytes += template.size;
                self.mem
                    .insert(key, Entry { key, partition, template: Arc::clone(&template) });
                return Some(template);
            }
        }
        None
    }

    /// Store a template of a partition in memory and queue it for the disk store (write-behind).
    pub fn store(&mut self, key: u128, partition: u128, template: Template) {
        self.stats.bytes_stored += template.size as u64;
        if let Some(disk) = &self.disk {
            disk.store(key, partition, &template);
        }
        self.mem_bytes += template.size;
        let entry = Entry { key, partition, template: Arc::new(template) };
        if self.shared {
            SHARED.lock().unwrap().insert(key, entry.clone());
        }
        self.mem.insert(key, entry);
    }

    /// Block until all queued disk writes have been performed.
    pub fn flush(&self) {
        if let Some(disk) = &self.disk {
            disk.flush();
        }
    }

    pub fn mem_len(&self) -> usize {
        self.mem.len()
    }

    pub fn mem_bytes(&self) -> usize {
        self.mem_bytes
    }

    pub fn disk_dir(&self) -> Option<&std::path::Path> {
        self.disk.as_ref().map(|d| d.dir())
    }
}

impl Default for FunctionCache {
    fn default() -> Self {
        Self::new()
    }
}

/// The digest of a partition under a secret: SipHash-1-3 with 128 bits, keyed with the secret.
fn digest_partition(secret: &secret::KeySecret, partition: &[u8]) -> u128 {
    use std::hash::Hasher;

    use siphasher::sip128::{Hasher128, SipHasher13};
    let mut hasher = SipHasher13::new_with_key(secret);
    hasher.write(b"fncache-partition");
    hasher.write_u64(partition.len() as u64);
    hasher.write(partition);
    hasher.finish128().as_u128()
}
