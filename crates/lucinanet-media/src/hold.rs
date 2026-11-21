/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Juno's screen stays on while a video plays, as Windows' media playback asks for `ES_DISPLAY_REQUIRED` and a Wayland browser
//! takes an idle inhibitor.
//!
//! The protocol is the shell's (LucinaOS `lucina_app::hold`, juno-power): the shell gives every app a pipe as the descriptor in
//! `LUCINA_HOLD_FD`; the line `display-required on <reason>` holds the screen on, `display-required off` lets go, and the hold ends
//! with the app; the reason is cut to 60 characters by the shell. LucinaNet does not link `lucina-app`, so the two lines are written
//! here, only on changes. One hold for the browser (the shell keeps one per app), on while any player with pictures plays with
//! sound (not paused, not ended, not muted): the browser rule juno-power gave. A muted autoplay background video does not keep the
//! screen on; full screen would, but the backend does not see it. If the shell has gone, a write error stops further writes.

use std::collections::BTreeSet;
use std::sync::Mutex;

struct Holds {
    playing: BTreeSet<usize>,
    fd: Option<i32>,
    looked: bool,
}

static HOLDS: Mutex<Holds> = Mutex::new(Holds { playing: BTreeSet::new(), fd: None, looked: false });

fn descriptor(h: &mut Holds) -> Option<i32> {
    if !h.looked {
        h.looked = true;
        // The shell gives descriptor 3 and says so; anything else is not its pipe.
        h.fd = std::env::var("LUCINA_HOLD_FD").ok().and_then(|v| v.parse().ok()).filter(|fd: &i32| *fd == 3);
        if let Some(fd) = h.fd {
            // The hold must not outlive this program in a child it starts.
            // SAFETY: fcntl on a descriptor number; on a closed one it fails and changes nothing.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
    h.fd
}

/// `false` when the pipe refused it: the shell has gone, and nothing more is written.
fn write_line(fd: i32, line: &str) -> bool {
    // SAFETY: writes from a live buffer of the given length; a closed or broken pipe fails, which the caller handles.
    let n = unsafe { libc::write(fd, line.as_ptr().cast(), line.len()) };
    n == line.len() as isize
}

/// Player `id` plays pictures now (`true`) or not.
pub fn set(id: usize, playing: bool) {
    let mut h = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
    let before = !h.playing.is_empty();
    if playing {
        h.playing.insert(id);
    } else {
        h.playing.remove(&id);
    }
    let after = !h.playing.is_empty();
    if before == after {
        return;
    }
    if let Some(fd) = descriptor(&mut h)
        && !write_line(fd, if after { "display-required on playing video\n" } else { "display-required off\n" })
    {
        log::debug!("lucinanet-media: the screen hold pipe refused a line; the shell has gone");
        h.fd = None;
    }
    log::info!("lucinanet-media: screen hold {}", if after { "on" } else { "off" });
}

#[cfg(test)]
pub fn held() -> bool {
    !HOLDS.lock().unwrap().playing.is_empty()
}
