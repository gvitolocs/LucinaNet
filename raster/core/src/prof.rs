/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `lucina_prof.h`: where the rasterizer's time goes. Off unless `LUCINANET_SWGL_PROFILE` is set; then every 2 s it
//! prints one line a key (shader and blend mode, or an operation) with calls, instances, shaded pixels and
//! milliseconds, and resets. Same keys and format as swgl's, so the two tables line up. The last window is printed at
//! exit (as swgl's, through `atexit`).

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

struct Slot {
    name: String,
    blend: i32,
    calls: u64,
    instances: u64,
    pixels: u64,
    ns: u64,
}

struct Table {
    slots: Vec<Slot>,
    last_dump: u64,
}

static TABLE: Mutex<Table> = Mutex::new(Table { slots: Vec::new(), last_dump: 0 });
static ENABLED: AtomicI32 = AtomicI32::new(-1);

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn flush_at_exit() {
    let mut t = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    if !t.slots.is_empty() {
        let now = now();
        dump_locked(&mut t, now);
    }
}

fn now() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    // Not 0: a start of 0 means "off", as in swgl.
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64 + 1
}

/// `lucina_prof::on()`.
#[inline]
pub fn on() -> bool {
    let e = ENABLED.load(Ordering::Relaxed);
    if e >= 0 {
        return e == 1;
    }
    let e = i32::from(std::env::var_os("LUCINANET_SWGL_PROFILE").is_some());
    if ENABLED.compare_exchange(-1, e, Ordering::Relaxed, Ordering::Relaxed).is_ok() && e == 1 {
        // SAFETY: registers a plain `extern "C"` function with libc.
        unsafe { atexit(flush_at_exit) };
    }
    e == 1
}

/// `LUCINA_PROF_BEGIN()`: the start time, or 0 when profiling is off.
#[inline]
pub fn begin() -> u64 {
    if on() {
        now()
    } else {
        0
    }
}

/// `LUCINA_PROF_END(name, blend, instances, pixels)`.
#[inline]
pub fn end(start: u64, name: &str, blend: i32, instances: u64, pixels: u64) {
    if start != 0 {
        add(name, blend, instances, pixels, start, now());
    }
}

fn dump_locked(t: &mut Table, now: u64) {
    t.slots.sort_by_key(|s| core::cmp::Reverse(s.ns));
    let total: f64 = t.slots.iter().map(|s| s.ns as f64).sum();
    eprintln!("swgl-prof window {:.0} ms, swgl {:.1} ms", (now - t.last_dump) as f64 / 1e6, total / 1e6);
    for s in &t.slots {
        eprintln!(
            "swgl-prof {:<48} blend {:3} calls {:6} inst {:7} px {:10} ms {:8.2}",
            s.name,
            s.blend,
            s.calls,
            s.instances,
            s.pixels,
            s.ns as f64 / 1e6
        );
    }
    t.slots.clear();
    t.last_dump = now;
}

fn add(name: &str, blend: i32, instances: u64, pixels: u64, start: u64, end: u64) {
    let mut t = TABLE.lock().unwrap_or_else(|e| e.into_inner());
    if t.last_dump == 0 {
        t.last_dump = start;
    }
    let i = match t.slots.iter().position(|s| s.name == name && s.blend == blend) {
        Some(i) => i,
        None => {
            t.slots.push(Slot { name: name.to_owned(), blend, calls: 0, instances: 0, pixels: 0, ns: 0 });
            t.slots.len() - 1
        }
    };
    let s = &mut t.slots[i];
    s.calls += 1;
    s.instances += instances;
    s.pixels += pixels;
    s.ns += end - start;
    if end - t.last_dump > 2_000_000_000 {
        dump_locked(&mut t, end);
    }
}
