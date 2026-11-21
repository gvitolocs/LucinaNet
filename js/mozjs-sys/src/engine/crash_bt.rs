/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Crash backtraces for engine work (LucinaNet): with `LUCINANET_JS_CRASH_BT=1`, a fatal signal (SIGSEGV, SIGBUS, SIGILL,
//! SIGABRT) prints the signal, the faulting address, the thread and a backtrace to stderr, then takes the default action.
//! Rare crashes that a debugger's timing hides can then be read from an ordinary run's log. Capturing a backtrace in a
//! signal handler is not async-signal-safe (it may deadlock if the crash was inside the allocator); this is a diagnostic
//! switch, off by default.

use ::std::sync::atomic::{AtomicBool, Ordering};

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Whether `LUCINANET_JS_CRASH_BT` is set to anything but empty or `0`.
fn enabled() -> bool {
    match ::std::env::var("LUCINANET_JS_CRASH_BT") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}

/// Install the handlers if `LUCINANET_JS_CRASH_BT` is set (not empty, not `0`). Only the first call in the process does
/// anything. Besides the fatal-signal handlers this chains a panic hook that prints `lucinanet-panic:` and a symbolized
/// backtrace for every Rust panic: the first engine context is created after the embedder (servoshell) has set its own
/// hook, so the previous hook (and its message output) is kept and runs first.
pub fn maybe_install() {
    if !enabled() {
        return;
    }
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    install_panic_hook();
    for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGABRT] {
        // SAFETY: a zeroed sigaction with a handler, SA_SIGINFO, SA_ONSTACK (the thread's alternate stack, if any, so a
        // stack overflow can still report) and SA_RESETHAND (the second delivery takes the default action).
        unsafe {
            let mut action: libc::sigaction = ::std::mem::zeroed();
            action.sa_sigaction = handler as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, ::std::ptr::null_mut());
        }
    }
}

/// Chain a panic hook that adds the `lucinanet-panic:` line and a backtrace of at most [`MAX_PANIC_FRAMES`] frames.
fn install_panic_hook() {
    let previous = ::std::panic::take_hook();
    ::std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let thread = ::std::thread::current();
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string payload>");
        let backtrace = ::std::backtrace::Backtrace::force_capture().to_string();
        eprintln!(
            "{}",
            format_panic(thread.name().unwrap_or("?"), location.as_deref(), message, &backtrace)
        );
    }));
}

/// Frames kept in a panic backtrace.
pub(super) const MAX_PANIC_FRAMES: usize = 40;

/// The `lucinanet-panic:` report: one line, then the backtrace cut to [`MAX_PANIC_FRAMES`] frames.
fn format_panic(thread: &str, location: Option<&str>, message: &str, backtrace: &str) -> String {
    format!(
        "lucinanet-panic: thread '{thread}' panicked at {}: {message}\n{}",
        location.unwrap_or("<unknown location>"),
        truncate_frames(backtrace, MAX_PANIC_FRAMES)
    )
}

/// Keep the first `max` frames of a `Backtrace` display (`  N: symbol` lines, each maybe followed by an `at file:line`
/// line), and say how many were dropped.
pub(super) fn truncate_frames(backtrace: &str, max: usize) -> String {
    let mut out = String::new();
    let mut frames = 0usize;
    let mut dropped = 0usize;
    for line in backtrace.lines() {
        let trimmed = line.trim_start();
        let is_frame = trimmed
            .split_once(':')
            .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if is_frame {
            frames += 1;
        }
        if frames > max {
            dropped += is_frame as usize;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if dropped > 0 {
        out.push_str(&format!("  ... {dropped} more frames\n"));
    }
    out
}

#[cfg(target_env = "gnu")]
extern "C" {
    // glibc <execinfo.h>: walk the stack and write it to a file descriptor without allocating.
    fn backtrace(buffer: *mut *mut libc::c_void, size: libc::c_int) -> libc::c_int;
    fn backtrace_symbols_fd(buffer: *const *mut libc::c_void, size: libc::c_int, fd: libc::c_int);
}

#[super::poison::engine_shim(exempt = "the fatal-signal handler: runs after the crash in signal context")]
extern "C" fn handler(signal: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid siginfo for SA_SIGINFO handlers.
    let address = unsafe { (*info).si_addr() };
    // First the raw frames (async-signal-safe: no allocation), resolvable with addr2line if the symbolized backtrace
    // below deadlocks in the allocator. glibc only: musl (Juno's static builds) has no <execinfo.h>, so there the
    // backtrace below is all there is.
    #[cfg(target_env = "gnu")]
    raw_frames();
    let thread = ::std::thread::current();
    eprintln!(
        "\nlucinanet-crash: signal {signal} at address {address:?} in thread {:?}\n{}",
        thread.name().unwrap_or("?"),
        ::std::backtrace::Backtrace::force_capture()
    );
    // SA_RESETHAND restored the default action: raise again to take it (core dump, exit status).
    // SAFETY: raising a signal has no preconditions.
    unsafe {
        libc::raise(signal);
    }
}

/// The raw frames to stderr, without allocating.
#[cfg(target_env = "gnu")]
fn raw_frames() {
    let mut frames = [::std::ptr::null_mut::<libc::c_void>(); 64];
    // SAFETY: the buffer holds 64 frames; fd 2 is stderr.
    unsafe {
        let header = b"\nlucinanet-crash: raw frames\n";
        libc::write(2, header.as_ptr().cast(), header.len());
        let n = backtrace(frames.as_mut_ptr(), frames.len() as libc::c_int);
        backtrace_symbols_fd(frames.as_ptr(), n, 2);
        // backtrace_symbols_fd prints `<unknown>` for a stripped or PIE binary without dynamic symbols. Follow it with
        // `module+offset` per frame (dladdr), which `addr2line -e <module> <offset>` resolves against the build's debug
        // info. dladdr is not strictly async-signal-safe; it comes after the lines above on purpose.
        let header = b"lucinanet-crash: frame module+offset\n";
        libc::write(2, header.as_ptr().cast(), header.len());
        for &frame in &frames[..n.max(0) as usize] {
            let mut info: libc::Dl_info = ::std::mem::zeroed();
            if libc::dladdr(frame, &mut info) == 0 || info.dli_fname.is_null() {
                continue;
            }
            let offset = frame as usize - info.dli_fbase as usize;
            let name = ::std::ffi::CStr::from_ptr(info.dli_fname).to_bytes();
            libc::write(2, b"  ".as_ptr().cast(), 2);
            libc::write(2, name.as_ptr().cast(), name.len());
            libc::write(2, b"+0x".as_ptr().cast(), 3);
            let mut digits = [0u8; 16];
            let mut i = digits.len();
            let mut value = offset;
            loop {
                i -= 1;
                digits[i] = b"0123456789abcdef"[value & 0xf];
                value >>= 4;
                if value == 0 {
                    break;
                }
            }
            libc::write(2, digits[i..].as_ptr().cast(), digits.len() - i);
            libc::write(2, b"\n".as_ptr().cast(), 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_has_the_panic_line_and_cuts_frames() {
        let mut bt = String::new();
        for n in 0..50 {
            bt.push_str(&format!("{n:>4}: sym{n}\n             at src/lib.rs:{n}:1\n"));
        }
        let report = format_panic("Script#1", Some("a.rs:158"), "already mutably borrowed", &bt);
        assert!(report.starts_with(
            "lucinanet-panic: thread 'Script#1' panicked at a.rs:158: already mutably borrowed\n"
        ));
        assert!(report.contains("sym39"));
        assert!(!report.contains("sym40"));
        assert!(report.contains("... 10 more frames"));
    }

    /// Run in a child process by `hook_prints_panic_line`.
    #[test]
    #[ignore]
    fn child_panics() {
        maybe_install();
        let _ = ::std::panic::catch_unwind(|| panic!("deliberate test panic"));
    }

    #[test]
    fn hook_prints_panic_line() {
        let exe = ::std::env::current_exe().unwrap();
        let out = ::std::process::Command::new(exe)
            .args(["--exact", "engine::crash_bt::tests::child_panics", "--ignored", "--nocapture"])
            .env("LUCINANET_JS_CRASH_BT", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("lucinanet-panic: thread '") && stderr.contains("deliberate test panic"),
            "stderr was: {stderr}"
        );
        assert!(stderr.contains("panicked at"), "the previous hook's message is kept: {stderr}");
    }
}
