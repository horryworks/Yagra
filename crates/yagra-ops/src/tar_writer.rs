// SPDX-License-Identifier: AGPL-3.0-only
//! Writing a tar archive — the only half of tar the bundles need (ADR-214 decision 1).
//!
//! The support bundle and the remote-site bundle write archives and never read one in production,
//! so the `tar` crate (and `rustix`, `xattr` and `filetime` with it) is a dev-dependency now: the
//! tests read our output back through it. The bytes are the ones `tar::Builder::append_data` wrote
//! with a `Header::new_gnu()` — `the_bytes_are_the_tar_crates_bytes` compares them — so an archive
//! downloaded before and after the change unpacks the same way:
//!
//! - a GNU header (`ustar ` + ` \0`), the name, mode, size and mtime set, everything else zero —
//!   the owner fields included, as `new_gnu` left them;
//! - a name longer than the 100-byte field goes first in a `././@LongLink` entry of type `L`
//!   (NUL-terminated), and the real header carries its first 100 bytes;
//! - data padded to 512-byte blocks, and two zero blocks to end the archive.
//!
//! A name that is empty, absolute or contains `..` is refused rather than written: every name here
//! is chosen by Yagra, so one of those is a defect, and an archive is the wrong place to find out.

use std::io::{self, Write};

const BLOCK: usize = 512;
/// The `name` field of a header.
const NAME_LEN: usize = 100;
/// Sizes from here up need tar's binary number encoding; no bundle file comes near it.
const MAX_OCTAL_SIZE: u64 = 8_589_934_592;

/// An archive being written to `out`. Call [`TarWriter::finish`] to end it.
pub struct TarWriter<W: Write> {
    out: W,
}

impl<W: Write> TarWriter<W> {
    /// Start an archive.
    pub fn new(out: W) -> Self {
        Self { out }
    }

    /// Append one regular file.
    ///
    /// # Errors
    /// A name that is empty, absolute or contains `..`; a file of 8 GiB or more; or an error from
    /// the underlying writer.
    pub fn append(&mut self, path: &str, bytes: &[u8], mode: u32, mtime: u64) -> io::Result<()> {
        check_path(path)?;
        let size = bytes.len() as u64;
        if size >= MAX_OCTAL_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a tar entry of 8 GiB or more is not supported",
            ));
        }
        let name = path.as_bytes();
        if name.len() > NAME_LEN {
            let mut long = gnu_header();
            long[..13].copy_from_slice(b"././@LongLink");
            octal(&mut long[100..108], 0o644);
            octal(&mut long[108..116], 0);
            octal(&mut long[116..124], 0);
            octal(&mut long[124..136], name.len() as u64 + 1);
            octal(&mut long[136..148], 0);
            long[156] = b'L';
            checksum(&mut long);
            let mut data = name.to_vec();
            data.push(0);
            self.write_entry(&long, &data)?;
        }
        let mut header = gnu_header();
        let shown = &name[..name.len().min(NAME_LEN)];
        header[..shown.len()].copy_from_slice(shown);
        octal(&mut header[100..108], u64::from(mode));
        octal(&mut header[124..136], size);
        octal(&mut header[136..148], mtime);
        checksum(&mut header);
        self.write_entry(&header, bytes)
    }

    /// End the archive with its two zero blocks and hand the writer back.
    ///
    /// # Errors
    /// An error from the underlying writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.write_all(&[0u8; BLOCK * 2])?;
        Ok(self.out)
    }

    fn write_entry(&mut self, header: &[u8; BLOCK], data: &[u8]) -> io::Result<()> {
        self.out.write_all(header)?;
        self.out.write_all(data)?;
        let pad = (BLOCK - data.len() % BLOCK) % BLOCK;
        self.out.write_all(&[0u8; BLOCK][..pad])
    }
}

/// A zeroed header carrying the GNU magic, as `tar::Header::new_gnu` builds it.
fn gnu_header() -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[257..263].copy_from_slice(b"ustar ");
    h[263..265].copy_from_slice(b" \0");
    // `new_gnu` also sets the mtime to 0; every caller here sets it again.
    octal(&mut h[136..148], 0);
    h
}

/// Right-aligned, zero-padded octal ending in NUL — the `tar` crate's `octal_into`.
fn octal(field: &mut [u8], value: u64) {
    let digits = format!("{value:o}");
    let width = field.len() - 1;
    let digits = digits.as_bytes();
    let start = width.saturating_sub(digits.len());
    field[..start].fill(b'0');
    field[start..width].copy_from_slice(&digits[digits.len().saturating_sub(width)..]);
    field[width] = 0;
}

/// The header checksum: the byte sum with the checksum field read as eight spaces.
fn checksum(h: &mut [u8; BLOCK]) {
    let sum: u32 = h[..148].iter().map(|&b| u32::from(b)).sum::<u32>()
        + 8 * u32::from(b' ')
        + h[156..].iter().map(|&b| u32::from(b)).sum::<u32>();
    octal(&mut h[148..156], u64::from(sum));
}

fn check_path(path: &str) -> io::Result<()> {
    let refused = path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|c| c == "..");
    if refused {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to write tar entry {path:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `tar::Builder` writes for the same entries, as the two bundles called it.
    fn tar_crate(entries: &[(&str, &[u8], u32, u64)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (path, bytes, mode, mtime) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(*mode);
            h.set_mtime(*mtime);
            b.append_data(&mut h, path, *bytes).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn ours(entries: &[(&str, &[u8], u32, u64)]) -> Vec<u8> {
        let mut w = TarWriter::new(Vec::new());
        for (path, bytes, mode, mtime) in entries {
            w.append(path, bytes, *mode, *mtime).unwrap();
        }
        w.finish().unwrap()
    }

    /// 🚨 The property the whole module rests on: an archive is byte-for-byte what the `tar` crate
    /// wrote, across the shapes the bundles produce — a short name, a name of exactly 100 bytes, a
    /// long one, an empty file, a file of exactly one block, the 0600 `.env`.
    #[test]
    fn the_bytes_are_the_tar_crates_bytes() {
        let exact = "d/".repeat(49) + "ab"; // 100 bytes
        let long = format!(
            "pollers/{}/logs/yagra-poller.2026-10-26-01.log",
            "p".repeat(120)
        );
        let block = vec![b'x'; 512];
        let entries: Vec<(&str, &[u8], u32, u64)> = vec![
            ("MANIFEST.json", b"{\"a\":1}\n", 0o644, 1_761_436_800),
            (".env", b"YAGRA_POLLER_TOKEN=secret\n", 0o600, 1_761_436_800),
            (exact.as_str(), b"exact", 0o644, 0),
            (long.as_str(), b"long name", 0o644, 42),
            ("empty.txt", b"", 0o644, 7),
            ("certs/server-cert.pem", &block, 0o644, 1),
        ];
        assert_eq!(ours(&entries), tar_crate(&entries));
    }

    #[test]
    fn an_archive_reads_back_with_names_modes_times_and_bodies() {
        let long = "x/".repeat(70) + "end.txt";
        let bytes = ours(&[
            ("a.txt", b"hello", 0o600, 99),
            (long.as_str(), b"world", 0o644, 5),
        ]);
        let mut archive = tar::Archive::new(bytes.as_slice());
        let got: Vec<(String, u32, u64, String)> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let path = e.path().unwrap().display().to_string();
                let (mode, mtime) = (e.header().mode().unwrap(), e.header().mtime().unwrap());
                let mut body = String::new();
                io::Read::read_to_string(&mut e, &mut body).unwrap();
                (path, mode, mtime, body)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("a.txt".to_owned(), 0o600, 99, "hello".to_owned()),
                (long, 0o644, 5, "world".to_owned()),
            ]
        );
    }

    #[test]
    fn a_name_that_could_escape_the_extraction_directory_is_refused() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a\\b", "a\0b"] {
            let mut w = TarWriter::new(Vec::new());
            assert!(
                w.append(bad, b"x", 0o644, 0).is_err(),
                "{bad:?} was written"
            );
        }
        // A dot-dot inside a name is not a component and stays allowed.
        let mut w = TarWriter::new(Vec::new());
        assert!(w.append("a..b/c", b"x", 0o644, 0).is_ok());
    }

    #[test]
    fn octal_fields_match_the_tar_layout() {
        let mut f = [0u8; 8];
        octal(&mut f, 0o644);
        assert_eq!(&f, b"0000644\0");
        let mut f = [0u8; 12];
        octal(&mut f, 5);
        assert_eq!(&f, b"00000000005\0");
    }
}
