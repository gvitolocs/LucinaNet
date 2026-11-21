//! The secret that keys the function cache's hash.
//!
//! The key of a function is a keyed hash. Without the secret nobody can build two functions with
//! the same key, so a page cannot poison the cache for another page. The secret lives in the
//! store directory, so that a store is only ever read with the secret its records were written
//! under; without a directory it is random per process.

use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use rand::{RngCore, rngs::OsRng};

/// 128 random bits.
pub type KeySecret = [u8; 16];

/// The name of the secret file inside a store directory.
pub const SECRET_FILE_NAME: &str = "secret.key";

static PROCESS_SECRET: LazyLock<KeySecret> = LazyLock::new(random_secret);

/// A fresh secret from the operating system's randomness.
pub fn random_secret() -> KeySecret {
    let mut secret = [0u8; 16];
    OsRng.fill_bytes(&mut secret);
    secret
}

/// The secret shared by every context of this process that has no store directory.
pub fn process_secret() -> KeySecret {
    *PROCESS_SECRET
}

/// The secret of a store directory. A missing, unreadable or malformed secret file means a new
/// secret: the file is replaced, and the records written under the old one can no longer hit
/// (their keys were made with it) and age out of the store. Returns None if a secret cannot be
/// made durable, in which case the directory must not be used.
pub fn load_or_create(dir: &Path) -> Option<KeySecret> {
    let path = dir.join(SECRET_FILE_NAME);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(secret) = KeySecret::try_from(bytes.as_slice()) {
            return Some(secret);
        }
    }

    let secret = random_secret();
    std::fs::create_dir_all(dir).ok()?;

    // Temp file, fsync, rename, fsync of the directory: the secret file either has all of its
    // 16 bytes or does not exist, also after a power cut.
    let tmp = dir.join(format!("{SECRET_FILE_NAME}.tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(&secret)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(secret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fncache-secret-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn secret_is_created_once_and_private() {
        let dir = temp_dir("create");
        let first = load_or_create(&dir).unwrap();
        assert_eq!(load_or_create(&dir), Some(first));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SECRET_FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_secret_file_is_replaced() {
        let dir = temp_dir("malformed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SECRET_FILE_NAME), b"short").unwrap();
        let secret = load_or_create(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(SECRET_FILE_NAME)).unwrap(), secret);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secrets_differ() {
        assert_ne!(random_secret(), random_secret());
    }
}
