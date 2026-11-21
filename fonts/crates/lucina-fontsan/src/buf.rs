// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Behaviour derived from OTS (BSD-3-Clause, see LICENSES/BSD-3-Clause-OTS.txt).

//! Big-endian cursor over a byte slice, mirroring ots::Buffer (ots.h).

pub struct Buf<'a> {
    data: &'a [u8],
    off: usize,
}

impl<'a> Buf<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Buf { data, off: 0 }
    }
    pub fn offset(&self) -> usize {
        self.off
    }
    pub fn set_offset(&mut self, off: usize) {
        self.off = off;
    }
    pub fn len(&self) -> usize {
        self.data.len()
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.off
    }
    pub fn read_u8(&mut self) -> Option<u8> {
        let b = *self.data.get(self.off)?;
        self.off += 1;
        Some(b)
    }
    pub fn read_u16(&mut self) -> Option<u16> {
        let s = self.data.get(self.off..self.off + 2)?;
        self.off += 2;
        Some(u16::from_be_bytes([s[0], s[1]]))
    }
    pub fn read_s16(&mut self) -> Option<i16> {
        self.read_u16().map(|v| v as i16)
    }
    pub fn read_u32(&mut self) -> Option<u32> {
        let s = self.data.get(self.off..self.off + 4)?;
        self.off += 4;
        Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub fn read_s32(&mut self) -> Option<i32> {
        self.read_u32().map(|v| v as i32)
    }
    pub fn skip(&mut self, n: usize) -> bool {
        if self.remaining() < n {
            return false;
        }
        self.off += n;
        true
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.off..]
    }
}
