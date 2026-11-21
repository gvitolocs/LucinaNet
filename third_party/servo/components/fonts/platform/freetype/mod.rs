/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

pub mod font;

#[cfg(any(
    all(target_os = "linux", not(target_env = "ohos"), not(ohos_mock)),
    target_os = "freebsd"
))]
pub mod font_list;

#[cfg(target_os = "android")]
mod android {
    pub mod font_list;
    mod xml;
}
#[cfg(target_os = "android")]
pub use self::android::font_list;

#[cfg(any(target_env = "ohos", ohos_mock))]
// Upstream's OpenHarmony font list, whose mmap is reviewed upstream.
#[expect(unsafe_code)]
mod ohos {
    mod font_cache;
    pub mod font_list;
}
#[cfg(any(target_env = "ohos", ohos_mock))]
pub use self::ohos::font_list;

/// The bytes of a local font file: mapped, as a font collection can be tens of megabytes and only
/// some of its tables are read, or read into memory when the map's premise does not hold.
pub(crate) enum FontFileBytes {
    Mapped(memmap2::Mmap),
    Read(Vec<u8>),
}

impl std::ops::Deref for FontFileBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Mapped(map) => map,
            Self::Read(bytes) => bytes,
        }
    }
}

/// Open a local font file read-only and map it, or read it if this process may write to it.
#[expect(unsafe_code)]
pub(crate) fn font_file_bytes(path: &std::path::Path) -> std::io::Result<FontFileBytes> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;

    let mut file = std::fs::File::open(path)?;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `path` is a NUL-terminated string that lives for the call.
    let writable = unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0;
    if writable {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        return Ok(FontFileBytes::Read(bytes));
    }
    // SAFETY: memmap2 needs the file not to be written in place or truncated while it is mapped.
    // This process can't write to it (checked just above), and the deployment premise is that
    // nothing else does either: the font directories (`LUCINANET_FONT_DIRS`, or the system's) hold
    // read-only system files, which are replaced only by a rename (a new inode, as a package
    // manager does), never written in place. The bytes are only ever read through `&[u8]`, with
    // every offset checked against the mapped length.
    unsafe { memmap2::Mmap::map(&file) }.map(FontFileBytes::Mapped)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::{FontFileBytes, font_file_bytes};

    #[test]
    fn font_file_bytes_maps_only_a_file_this_process_cannot_write() {
        let path =
            std::env::temp_dir().join(format!("servo-font-file-bytes-{}", std::process::id()));
        std::fs::write(&path, b"OTTO and some tables").unwrap();

        let bytes = font_file_bytes(&path).unwrap();
        assert!(matches!(bytes, FontFileBytes::Read(_)));
        assert_eq!(&*bytes, b"OTTO and some tables");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let bytes = font_file_bytes(&path).unwrap();
        // Root may write to any file, so it always reads.
        if running_as_root() {
            assert!(matches!(bytes, FontFileBytes::Read(_)));
        } else {
            assert!(matches!(bytes, FontFileBytes::Mapped(_)));
        }
        assert_eq!(&*bytes, b"OTTO and some tables");

        drop(bytes);
        std::fs::remove_file(&path).unwrap();
    }

    fn running_as_root() -> bool {
        std::fs::read_to_string("/proc/self/status").is_ok_and(|status| {
            status
                .lines()
                .any(|line| line.starts_with("Uid:") && line.split_whitespace().nth(2) == Some("0"))
        })
    }
}
