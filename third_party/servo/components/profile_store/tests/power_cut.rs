/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Power cuts, simulated at the level of the disk: every write and sync the store makes is logged, and a cut at any point leaves
//! what was synced plus any subset of the writes after it, each of those possibly torn at 512-byte sectors (a disk may write the
//! sectors of a request in any order and stop anywhere). After every such image the store must open, and hold exactly the state
//! after some commit: no earlier than the last commit made durable before the cut (a `flush` or `write_durable` that returned),
//! and no later than the last commit begun.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use profile_store::Store;
use profile_store::redb::backends::InMemoryBackend;
use profile_store::redb::{ReadableTable, StorageBackend, TableDefinition};

const TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("t");
const SECTOR: usize = 512;

#[derive(Clone, Debug)]
enum Op {
    Write(u64, Vec<u8>),
    SetLen(u64),
    Sync,
    /// Commit `n` returned (its state is `states[n]`).
    Committed(usize),
    /// Every commit up to `n` is durable: a flush or a durable write returned.
    Durable(usize),
}

#[derive(Debug, Default)]
struct Log {
    ops: Vec<Op>,
    image: Vec<u8>,
}

/// A disk that logs everything done to it.
#[derive(Clone, Debug, Default)]
struct LoggingDisk(Arc<Mutex<Log>>);

impl StorageBackend for LoggingDisk {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().image.len() as u64)
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let log = self.0.lock().unwrap();
        let start = offset as usize;
        let end = start + out.len();
        if end > log.image.len() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        out.copy_from_slice(&log.image[start..end]);
        Ok(())
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut log = self.0.lock().unwrap();
        log.image.resize(len as usize, 0);
        log.ops.push(Op::SetLen(len));
        Ok(())
    }
    fn sync_data(&self) -> Result<(), io::Error> {
        self.0.lock().unwrap().ops.push(Op::Sync);
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut log = self.0.lock().unwrap();
        let end = offset as usize + data.len();
        if end > log.image.len() {
            log.image.resize(end, 0);
        }
        log.image[offset as usize..end].copy_from_slice(data);
        log.ops.push(Op::Write(offset, data.to_vec()));
        Ok(())
    }
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn apply(image: &mut Vec<u8>, op: &Op) {
    match op {
        Op::Write(offset, data) => {
            let end = *offset as usize + data.len();
            if end > image.len() {
                image.resize(end, 0);
            }
            image[*offset as usize..end].copy_from_slice(data);
        },
        Op::SetLen(len) => image.resize(*len as usize, 0),
        _ => {},
    }
}

/// What is on the disk after a cut just before op `cut`: everything up to the last sync, then a random part of the rest.
fn crash_image(ops: &[Op], cut: usize, rng: &mut Rng) -> Vec<u8> {
    let last_sync = ops[..cut].iter().rposition(|op| matches!(op, Op::Sync)).map_or(0, |i| i + 1);
    let mut image = Vec::new();
    for op in &ops[..last_sync] {
        apply(&mut image, op);
    }
    for op in &ops[last_sync..cut] {
        match op {
            Op::Write(offset, data) => {
                if rng.chance(50) {
                    continue;
                }
                // Torn: each sector of the request lands or not.
                for (i, sector) in data.chunks(SECTOR).enumerate() {
                    if rng.chance(70) {
                        apply(&mut image, &Op::Write(offset + (i * SECTOR) as u64, sector.to_vec()));
                    }
                }
            },
            Op::SetLen(len) => {
                // A grown length may not have reached the disk; a shrink is never needed for safety.
                if rng.chance(50) || (*len as usize) < image.len() {
                    apply(&mut image, op);
                }
            },
            _ => {},
        }
    }
    image
}

fn read_back(image: Vec<u8>) -> Result<BTreeMap<u64, Vec<u8>>, String> {
    let backend = InMemoryBackend::new();
    backend.set_len(image.len() as u64).map_err(|e| e.to_string())?;
    backend.write(0, &image).map_err(|e| e.to_string())?;
    let store = Store::with_backend(backend).map_err(|e| format!("open: {e}"))?;
    let txn = store.read().map_err(|e| e.to_string())?;
    let mut map = BTreeMap::new();
    match txn.open_table(TABLE) {
        Ok(table) => {
            for entry in table.iter().map_err(|e| e.to_string())? {
                let (k, v) = entry.map_err(|e| e.to_string())?;
                map.insert(k.value(), v.value().to_vec());
            }
        },
        Err(profile_store::redb::TableError::TableDoesNotExist(_)) => {},
        Err(e) => return Err(e.to_string()),
    }
    Ok(map)
}

fn run(seed: u64, commits: usize, cuts: usize, images: usize) -> (usize, usize) {
    let disk = LoggingDisk::default();
    let mut rng = Rng(seed);
    let mut states: Vec<BTreeMap<u64, Vec<u8>>> = vec![BTreeMap::new()];
    {
        let store = Store::with_backend(disk.clone()).unwrap();
        // The empty state before any commit counts as durable once the file exists.
        store.write_durable(|_| Ok(())).unwrap();
        disk.0.lock().unwrap().ops.push(Op::Durable(0));
        for n in 1..=commits {
            let mut next = states.last().unwrap().clone();
            let ops = 1 + rng.below(20);
            let mut plan = Vec::new();
            for _ in 0..ops {
                let key = rng.below(300);
                if rng.chance(25) {
                    plan.push((key, None));
                    next.remove(&key);
                } else {
                    let len = rng.below(3000) as usize;
                    let value: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                    next.insert(key, value.clone());
                    plan.push((key, Some(value)));
                }
            }
            let durable = rng.chance(10);
            let body = |txn: &profile_store::redb::WriteTransaction| {
                let mut table = txn.open_table(TABLE)?;
                for (key, value) in &plan {
                    match value {
                        Some(value) => {
                            table.insert(key, value.as_slice())?;
                        },
                        None => {
                            table.remove(key)?;
                        },
                    }
                }
                Ok(())
            };
            if durable {
                store.write_durable(body).unwrap();
            } else {
                store.write(body).unwrap();
            }
            states.push(next);
            disk.0.lock().unwrap().ops.push(Op::Committed(n));
            if durable {
                disk.0.lock().unwrap().ops.push(Op::Durable(n));
            } else if rng.chance(15) {
                store.flush().unwrap();
                disk.0.lock().unwrap().ops.push(Op::Durable(n));
            }
        }
    }
    let ops = disk.0.lock().unwrap().ops.clone();
    let mut checked = 0;
    let mut distinct_outcomes = std::collections::BTreeSet::new();
    // A file cut before its first durable commit was never promised to open: Servo's engines then log and start empty.
    let created = ops.iter().position(|op| matches!(op, Op::Durable(0))).unwrap() + 1;
    for _ in 0..cuts {
        let cut = created + rng.below((ops.len() - created) as u64 + 1) as usize;
        // The earliest state allowed: the last durable mark before the cut.
        let lo = ops[..cut]
            .iter()
            .filter_map(|op| if let Op::Durable(n) = op { Some(*n) } else { None })
            .max()
            .unwrap_or(0);
        // The latest: the last commit returned, or the one in flight.
        let hi = ops[..cut]
            .iter()
            .filter_map(|op| if let Op::Committed(n) = op { Some(*n) } else { None })
            .max()
            .unwrap_or(0) +
            1;
        let hi = hi.min(commits);
        for _ in 0..images {
            let image = crash_image(&ops, cut, &mut rng);
            let found = read_back(image).unwrap_or_else(|e| panic!("seed {seed} cut {cut}: {e}"));
            let matched = (lo..=hi).find(|n| states[*n] == found);
            assert!(
                matched.is_some(),
                "seed {seed} cut {cut}: state matches no commit in {lo}..={hi} ({} keys)",
                found.len()
            );
            distinct_outcomes.insert(matched.unwrap());
            checked += 1;
        }
    }
    (checked, distinct_outcomes.len())
}

#[test]
fn power_cut_keeps_a_whole_commit_and_every_durable_one() {
    let mut total = 0;
    for seed in 1..=12u64 {
        let (checked, outcomes) = run(seed.wrapping_mul(0x9E3779B97F4A7C15), 150, 40, 4);
        total += checked;
        eprintln!("seed {seed}: {checked} images, {outcomes} distinct recovered commits");
    }
    assert!(total > 1000, "only {total} images checked");
}
