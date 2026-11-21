/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

#![deny(unsafe_code)]

//! A crate to hold very common types in Servo.
//!
//! You should almost never need to add a data type to this crate. Instead look for
//! a more shared crate that has fewer dependents.

pub mod cross_process_instant;
pub mod generic_channel;
#[macro_use]
pub mod id;
pub mod print_tree;
mod rope;
pub mod text;
pub mod threadboost;
pub mod threadpool;
mod unicode_block;

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use log::{trace, warn};
use malloc_size_of_derive::MallocSizeOf;
pub use rope::{Rope, RopeChars, RopeIndex, RopeMovement, RopeSlice};
use serde::{Deserialize, Serialize};
use webrender_api::Epoch as WebRenderEpoch;

pub fn read_json_from_file<T>(data: &mut T, config_dir: &Path, filename: &str)
where
    T: for<'de> Deserialize<'de>,
{
    let path = config_dir.join(filename);
    let display = path.display();

    let file = match File::open(&path) {
        Err(why) => {
            warn!("couldn't open {}: {}", display, why);
            return;
        },
        Ok(file) => file,
    };
    let buf_reader = BufReader::new(file);

    match serde_json::from_reader(buf_reader) {
        Ok(decoded_buffer) => *data = decoded_buffer,
        Err(why) => warn!("Could not deserialize file {}", why),
    }
}

pub fn write_json_to_file<T>(data: &T, config_dir: &Path, filename: &str)
where
    T: Serialize,
{
    let path = config_dir.join(filename);
    let display = path.display();
    // LucinaNet: Servo truncated the file in place and never synced, so a power cut left it empty
    // or torn. Write a temporary file, sync it, rename it over the target, then sync the
    // directory so the rename itself survives.
    let tmp_path = config_dir.join(format!("{filename}.tmp"));

    let result = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        {
            let mut writer = BufWriter::new(&mut file);
            serde_json::to_writer_pretty(&mut writer, data).map_err(std::io::Error::other)?;
            writer.flush()?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp_path, &path)?;
        // An error syncing the directory is ignored: the file itself is already durable.
        if let Ok(dir) = File::open(config_dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    // LucinaNet: Servo panicked on an I/O error here; a full disk must not take down the caller.
    if let Err(why) = result {
        log::error!("couldn't write {}: {}", display, why);
        return;
    }
    trace!("successfully wrote to {display}");
}

/// A struct for denoting the age of messages; prevents race conditions.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Deserialize,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    MallocSizeOf,
)]
pub struct Epoch(pub u32);

impl Epoch {
    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }
}

impl From<Epoch> for WebRenderEpoch {
    fn from(val: Epoch) -> Self {
        WebRenderEpoch(val.0)
    }
}

pub trait WebRenderEpochToU16 {
    fn as_u16(&self) -> u16;
}

impl WebRenderEpochToU16 for WebRenderEpoch {
    /// The value of this [`Epoch`] as a u16 value. Note that if this Epoch's
    /// value is more than u16::MAX, then the return value will be modulo
    /// u16::MAX.
    fn as_u16(&self) -> u16 {
        (self.0 % u16::MAX as u32) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Sample {
        n: u32,
        s: String,
    }

    #[test]
    fn write_then_read_round_trips_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let sample = Sample {
            n: 42,
            s: "héllo".to_owned(),
        };
        write_json_to_file(&sample, dir.path(), "sample.json");
        assert!(!dir.path().join("sample.json.tmp").exists());
        let mut read_back = Sample {
            n: 0,
            s: String::new(),
        };
        read_json_from_file(&mut read_back, dir.path(), "sample.json");
        assert_eq!(read_back, sample);
    }

    #[test]
    fn stale_tmp_from_a_crash_does_not_disturb_reading() {
        let dir = tempfile::tempdir().unwrap();
        let sample = Sample {
            n: 7,
            s: "kept".to_owned(),
        };
        write_json_to_file(&sample, dir.path(), "sample.json");
        // A power cut between creating the tmp file and the rename leaves it behind.
        std::fs::write(dir.path().join("sample.json.tmp"), b"torn garbage").unwrap();
        let mut read_back = Sample {
            n: 0,
            s: String::new(),
        };
        read_json_from_file(&mut read_back, dir.path(), "sample.json");
        assert_eq!(read_back, sample);
    }
}
