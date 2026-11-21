/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! A record: the value of one row, as the fields of a SQLite row were. Fields follow each other:
//! a fixed one is 8 bytes little-endian, a variable one is a 4-byte little-endian length and then
//! its bytes. Nothing in a record says what its fields are; each table reads them in the order it
//! wrote them, and a record with bytes left over, or too few, is corrupt.
//!
//! The bounds are in `bounds.rs`, which Verus checks: where a field may lie in a record of
//! a given length, and how long a record may grow. This file only reads and writes bytes at the
//! places those functions return.

use crate::StoreError;
use crate::rules::bounds::{FIXED, LEN_PREFIX, MAX_FIELD, field_span, fixed_span, record_may_grow};

/// Writes one record.
#[derive(Default)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    fn reserve(&mut self, add: usize) -> Result<(), StoreError> {
        if record_may_grow(self.bytes.len(), add) {
            Ok(())
        } else {
            Err(StoreError::corrupt(format!(
                "a record of {} bytes cannot take {add} more",
                self.bytes.len()
            )))
        }
    }

    pub fn u64(mut self, value: u64) -> Result<Self, StoreError> {
        self.reserve(FIXED)?;
        self.bytes.extend_from_slice(&value.to_le_bytes());
        Ok(self)
    }

    pub fn i64(self, value: i64) -> Result<Self, StoreError> {
        self.u64(value as u64)
    }

    pub fn bool(self, value: bool) -> Result<Self, StoreError> {
        self.u64(value as u64)
    }

    pub fn bytes(mut self, value: &[u8]) -> Result<Self, StoreError> {
        if value.len() > MAX_FIELD as usize {
            return Err(StoreError::corrupt(format!("a field of {} bytes", value.len())));
        }
        self.reserve(LEN_PREFIX + value.len())?;
        self.bytes.extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(value);
        Ok(self)
    }

    pub fn str(self, value: &str) -> Result<Self, StoreError> {
        self.bytes(value.as_bytes())
    }

    /// An optional field: a flag, then the field if there is one.
    pub fn opt_bytes(self, value: Option<&[u8]>) -> Result<Self, StoreError> {
        match value {
            Some(value) => self.bool(true)?.bytes(value),
            None => self.bool(false),
        }
    }

    pub fn opt_str(self, value: Option<&str>) -> Result<Self, StoreError> {
        self.opt_bytes(value.map(str::as_bytes))
    }

    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads one record, in the order it was written.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    fn short(&self, what: &str) -> StoreError {
        StoreError::corrupt(format!(
            "{what} at byte {} of a {}-byte record",
            self.at,
            self.bytes.len()
        ))
    }

    pub fn u64(&mut self) -> Result<u64, StoreError> {
        let end = fixed_span(self.bytes.len(), self.at).ok_or_else(|| self.short("no fixed field"))?;
        let mut word = [0u8; 8];
        word.copy_from_slice(&self.bytes[self.at..end]);
        self.at = end;
        Ok(u64::from_le_bytes(word))
    }

    pub fn i64(&mut self) -> Result<i64, StoreError> {
        self.u64().map(|value| value as i64)
    }

    pub fn bool(&mut self) -> Result<bool, StoreError> {
        match self.u64()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(StoreError::corrupt(format!("{other} as a flag"))),
        }
    }

    pub fn bytes(&mut self) -> Result<&'a [u8], StoreError> {
        let len = self.bytes.len();
        // The length prefix is itself a field of LEN_PREFIX bytes.
        let prefix = self
            .at
            .checked_add(LEN_PREFIX)
            .filter(|end| *end <= len)
            .ok_or_else(|| self.short("no length"))?;
        let mut word = [0u8; 4];
        word.copy_from_slice(&self.bytes[self.at..prefix]);
        let n = u32::from_le_bytes(word);
        let (start, end) =
            field_span(len, self.at, n).ok_or_else(|| self.short(&format!("a field of {n} bytes")))?;
        self.at = end;
        Ok(&self.bytes[start..end])
    }

    pub fn str(&mut self) -> Result<&'a str, StoreError> {
        let bytes = self.bytes()?;
        std::str::from_utf8(bytes).map_err(|_| StoreError::corrupt("text that is not UTF-8"))
    }

    pub fn string(&mut self) -> Result<String, StoreError> {
        self.str().map(str::to_owned)
    }

    pub fn opt_bytes(&mut self) -> Result<Option<&'a [u8]>, StoreError> {
        if self.bool()? { self.bytes().map(Some) } else { Ok(None) }
    }

    pub fn opt_string(&mut self) -> Result<Option<String>, StoreError> {
        if self.bool()? { self.string().map(Some) } else { Ok(None) }
    }

    /// The record must end here: a record longer than what its table reads is corrupt.
    pub fn end(self) -> Result<(), StoreError> {
        if self.at == self.bytes.len() {
            Ok(())
        } else {
            Err(self.short("bytes left over"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let bytes = Writer::new()
            .u64(7)
            .unwrap()
            .str("héllo")
            .unwrap()
            .opt_str(None)
            .unwrap()
            .opt_bytes(Some(&[1, 2, 3]))
            .unwrap()
            .bool(true)
            .unwrap()
            .i64(-5)
            .unwrap()
            .finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u64().unwrap(), 7);
        assert_eq!(r.str().unwrap(), "héllo");
        assert_eq!(r.opt_string().unwrap(), None);
        assert_eq!(r.opt_bytes().unwrap(), Some(&[1u8, 2, 3][..]));
        assert!(r.bool().unwrap());
        assert_eq!(r.i64().unwrap(), -5);
        r.end().unwrap();
    }

    #[test]
    fn refuses_short_long_and_lying_records() {
        let bytes = Writer::new().str("abc").unwrap().finish();
        // Every cut of a record is refused, never read past.
        for cut in 0..bytes.len() {
            let mut r = Reader::new(&bytes[..cut]);
            assert!(r.bytes().is_err(), "cut at {cut}");
        }
        // Bytes left over.
        let mut longer = bytes.clone();
        longer.push(0);
        let mut r = Reader::new(&longer);
        r.bytes().unwrap();
        assert!(r.end().is_err());
        // A length that says more than there is, up to u32::MAX.
        for n in [4u32, 1 << 20, MAX_FIELD, MAX_FIELD + 1, u32::MAX] {
            let mut lying = n.to_le_bytes().to_vec();
            lying.extend_from_slice(b"abc");
            assert!(Reader::new(&lying).bytes().is_err(), "length {n}");
        }
        // A flag that is neither 0 nor 1.
        let two = Writer::new().u64(2).unwrap().finish();
        assert!(Reader::new(&two).bool().is_err());
    }
}
