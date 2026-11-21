/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The one syscall this crate asks for that Linux has and the other platforms we build for do not.

use std::io;
use std::os::fd::RawFd;

/// A pipe with both ends O_NONBLOCK and FD_CLOEXEC. Linux: ONE pipe2 call exactly as before. Elsewhere: pipe() then fcntl on both ends.
#[doc(hidden)]
pub fn nonblocking_cloexec_pipe() -> std::io::Result<[std::os::fd::RawFd; 2]> {
    let mut fds: [RawFd; 2] = [-1, -1];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: pipe2 writes two descriptors into a live array of two.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fds)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        // SAFETY: pipe writes two descriptors into a live array of two.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            if let Err(e) = set_flag(fd, libc::F_GETFD, libc::F_SETFD, libc::FD_CLOEXEC)
                .and_then(|()| set_flag(fd, libc::F_GETFL, libc::F_SETFL, libc::O_NONBLOCK))
            {
                // SAFETY: closes the two descriptors this function opened and no one else has.
                for f in fds {
                    unsafe { libc::close(f) };
                }
                return Err(e);
            }
        }
        Ok(fds)
    }
}

/// The bit `bit` added to what `get` reads of `fd`, written back with `set`.
///
/// Two calls, so a process that forks between `pipe()` and this could keep a blocking descriptor or one that survives exec. Accepted
/// on the platforms without `pipe2`: nothing here forks in the window, and the wake pipe is written by the process's own threads.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn set_flag(fd: RawFd, get: libc::c_int, set: libc::c_int, bit: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own; what is read back is written to the same descriptor.
    let cur = unsafe { libc::fcntl(fd, get) };
    if cur < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above, `cur | bit` being this descriptor's own flags with one bit added.
    if unsafe { libc::fcntl(fd, set, cur | bit) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The flags of `fd`, so a platform without `pipe2` can be seen to have set them.
    fn flags(fd: RawFd, cmd: libc::c_int) -> libc::c_int {
        // SAFETY: fcntl reads the flags of a descriptor this test owns.
        let v = unsafe { libc::fcntl(fd, cmd) };
        assert!(v >= 0, "fcntl({fd}, {cmd}): {}", io::Error::last_os_error());
        v
    }

    #[test]
    fn the_pipe_works_and_is_nonblocking_and_close_on_exec() {
        let [read, write] = nonblocking_cloexec_pipe().expect("a pipe");
        assert!(read >= 0 && write >= 0 && read != write, "{read} {write}");
        for fd in [read, write] {
            assert_ne!(flags(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0, "fd {fd} closes on exec");
            assert_ne!(flags(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0, "fd {fd} does not block");
        }

        let b = 1u8;
        // SAFETY: one byte from a live local into a descriptor this test owns.
        assert_eq!(unsafe { libc::write(write, (&b as *const u8).cast(), 1) }, 1);
        let mut buf = [0u8; 4];
        // SAFETY: reads into a live buffer of its length from a descriptor this test owns.
        assert_eq!(unsafe { libc::read(read, buf.as_mut_ptr().cast(), 1) }, 1);
        assert_eq!(buf[0], 1);

        // The pipe is empty now, and a non-blocking read says so instead of sleeping.
        // SAFETY: as above.
        assert_eq!(unsafe { libc::read(read, buf.as_mut_ptr().cast(), 1) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EAGAIN));

        // SAFETY: closes the two descriptors this test owns, once.
        unsafe {
            libc::close(read);
            libc::close(write);
        }
        for fd in [read, write] {
            // SAFETY: fcntl on a descriptor this test just closed; the number is nobody else's here.
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1, "fd {fd} is closed");
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF), "fd {fd}");
        }
    }
}
