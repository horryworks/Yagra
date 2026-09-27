// SPDX-License-Identifier: AGPL-3.0-only
//! Replacing a file so that no reader ever sees half of it (ADR-184).
//!
//! Core hands files to other containers: the WebUI and bus certificates, the NATS configuration,
//! and the request, switch and selection files the updater sidecar reads. Each used to spell the
//! same sequence out, and the copies had drifted — two created the temporary file at its *final*
//! mode, so a private key the final mode makes group-readable was group-readable before it was
//! complete, and the three updater files were never flushed to disk before the rename that
//! published them.
//!
//! What [`write_atomically`] does, in order, and why each step is there:
//! 1. create `<name>.tmp` beside the destination at `0600`, so nobody the final mode would admit
//!    can read a half-written file;
//! 2. write and `fsync`, so a crash after the rename cannot leave the destination pointing at bytes
//!    that never reached the disk;
//! 3. set the final mode explicitly, because the create mode is narrowed by the umask;
//! 4. rename over the destination, which is atomic within one filesystem.
//!
//! The streamed upgrade bundle (`upgrade::BundleWriter`) is not written through here: it arrives in
//! chunks over an async body, and finishes with the same `fsync`-then-rename on its own handle.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The temporary name: the whole file name plus `.tmp`, so two destinations that differ only by
/// extension (`server.pem`, `server.key`) never share one.
fn temporary(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    dst.with_file_name(name)
}

/// Replace `dst` with `body`, leaving it at `mode` (ignored off Unix).
///
/// # Errors
/// The first io error; the destination is untouched unless the rename itself succeeded.
pub(crate) fn write_atomically(dst: &Path, body: &[u8], mode: u32) -> std::io::Result<()> {
    let tmp = temporary(dst);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    std::fs::rename(&tmp, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_replaced_whole_and_leaves_no_temporary_behind() {
        let dir = std::env::temp_dir().join(format!("yagra-atomic-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("server.pem");
        std::fs::write(&dst, "old").unwrap();
        write_atomically(&dst, b"new", 0o640).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(names, vec![std::ffi::OsString::from("server.pem")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "the final mode is set, not left to the umask");
        }
    }

    #[test]
    fn two_names_that_differ_by_extension_get_two_temporaries() {
        assert_ne!(
            temporary(Path::new("/d/server.pem")),
            temporary(Path::new("/d/server.key"))
        );
        assert_eq!(
            temporary(Path::new("/d/request")),
            Path::new("/d/request.tmp")
        );
    }

    /// Files allowed to rename a file into place themselves.
    const RENAMES_ITS_OWN: &[&str] = &["atomic_file.rs"];

    /// ADR-184: nobody else in this crate publishes a file by renaming a temporary over it.
    ///
    /// The needle is the blocking `rename`; the upgrade bundle's async one is the declared exception
    /// in the module doc and is spelled differently.
    #[test]
    fn no_other_module_renames_a_temp_file_over_a_destination() {
        let files = crate::module_source::crate_code();
        assert!(files.len() >= 150, "only {} files were read", files.len());
        let needle = format!("{}::rename(", "std::fs");
        let offenders: Vec<&str> = files
            .iter()
            .filter(|(name, code)| {
                !RENAMES_ITS_OWN.contains(&name.as_str()) && code.contains(needle.as_str())
            })
            .map(|(name, _)| name.as_str())
            .collect();
        assert!(
            offenders.is_empty(),
            "{offenders:?} rename a file into place by hand. Use `atomic_file::write_atomically`, \
             which creates it private, flushes it, and only then publishes it"
        );
        let callers = files
            .iter()
            .filter(|(_, code)| code.contains(&format!("{}(", "write_atomically")))
            .count();
        assert!(
            callers >= 4,
            "only {callers} files write through this module"
        );
    }
}
