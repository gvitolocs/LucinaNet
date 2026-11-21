/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The profile store under real power cuts on Juno (QEMU, LucinaOS scripts/qemu-juno-disk.sh mode `store`).
//!
//! `store_cut check <file>`: opens the store and checks that it holds exactly the state after some commit, printing
//! `store-cut: recovered <n>` or `store-cut: BAD <why>`.
//! `store_cut write <file>`: goes on committing from there until killed, printing `store-cut: committed <n>` after each commit
//! returns and `store-cut: durable <n>` after each flush returns. The host checks that the next boot recovers a commit no older
//! than the last `durable` and no newer than the last `committed` plus one.
//!
//! Commit n writes key n (a value derived from n, 100 to 4000 bytes) and `HEAD` = n, and deletes key n - 50: so the state after
//! commit n is keys max(1, n - 49) ..= n, each with its value, and nothing else.

use std::path::Path;
use std::process::ExitCode;

use profile_store::Store;
use profile_store::redb::{ReadableTableMetadata, TableDefinition};

const DATA: TableDefinition<u64, &[u8]> = TableDefinition::new("data");
const META: TableDefinition<&str, u64> = TableDefinition::new("meta");
const KEEP: u64 = 50;

fn value(n: u64) -> Vec<u8> {
    let mut x = n.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    let len = 100 + (x % 3900) as usize;
    (0..len)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545F4914F6CDD1D) >> 56) as u8
        })
        .collect()
}

fn check(store: &Store) -> Result<u64, String> {
    let txn = store.read().map_err(|e| e.to_string())?;
    let head = match txn.open_table(META) {
        Ok(meta) => meta.get("HEAD").map_err(|e| e.to_string())?.map(|v| v.value()).unwrap_or(0),
        Err(profile_store::redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(e.to_string()),
    };
    let data = txn.open_table(DATA).map_err(|e| e.to_string())?;
    let first = head.saturating_sub(KEEP - 1).max(1);
    let expected = if head == 0 { 0 } else { head - first + 1 };
    if data.len().map_err(|e| e.to_string())? != expected {
        return Err(format!("head {head}: {} keys, expected {expected}", data.len().unwrap_or(0)));
    }
    for n in first..=head {
        match data.get(n).map_err(|e| e.to_string())? {
            Some(v) if v.value() == value(n).as_slice() => {},
            Some(_) => return Err(format!("head {head}: key {n} has the wrong value")),
            None => return Err(format!("head {head}: key {n} missing")),
        }
    }
    Ok(head)
}

fn write(store: &Store, from: u64) -> Result<(), String> {
    let mut n = from;
    loop {
        n += 1;
        store
            .write(|txn| {
                let mut data = txn.open_table(DATA)?;
                data.insert(n, value(n).as_slice())?;
                if n > KEEP {
                    data.remove(n - KEEP)?;
                }
                let mut meta = txn.open_table(META)?;
                meta.insert("HEAD", n)?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        println!("store-cut: committed {n}");
        if n % 5 == 0 {
            store.flush().map_err(|e| e.to_string())?;
            println!("store-cut: durable {n}");
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (Some(mode), Some(file)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: store_cut check|write <file>");
        return ExitCode::from(2);
    };
    let started = std::time::Instant::now();
    let store = match Store::open(Path::new(file)) {
        Ok(store) => store,
        Err(e) => {
            println!("store-cut: BAD open: {e}");
            return ExitCode::FAILURE;
        },
    };
    let head = match check(&store) {
        Ok(head) => head,
        Err(e) => {
            println!("store-cut: BAD {e}");
            return ExitCode::FAILURE;
        },
    };
    println!("store-cut: recovered {head} (open and check {} ms)", started.elapsed().as_millis());
    if mode == "write" {
        if let Err(e) = write(&store, head) {
            println!("store-cut: BAD write: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
