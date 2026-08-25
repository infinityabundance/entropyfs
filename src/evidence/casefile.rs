//! Deterministic casefiles (§50): a self-describing input corpus.
//!
//! A casefile is a versioned envelope (magic + version + entry count)
//! followed by length-prefixed (name, sha256, bytes) entries. It makes a
//! benchmark corpus reproducible: the hash binds the bytes, the name is
//! descriptive, and the envelope is explicit little-endian (same
//! discipline as the on-disk format — evidence artifacts must not rot).

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::path::Path;

/// Casefile magic: `EFCS`.
pub const CASEFILE_MAGIC: [u8; 4] = *b"EFCS";
/// Casefile format version.
pub const CASEFILE_VERSION: u8 = 1;

/// One corpus entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseEntry {
    /// Descriptive name.
    pub name: Vec<u8>,
    /// SHA-256 of the bytes (binding).
    pub sha256: [u8; 32],
    /// The bytes.
    pub bytes: Vec<u8>,
}

impl CaseEntry {
    /// Build an entry, computing the hash.
    pub fn new(name: impl Into<Vec<u8>>, bytes: Vec<u8>) -> Self {
        let sha256 = blake3::hash(&bytes).as_bytes().to_owned(); // 32-byte digest
        Self {
            name: name.into(),
            sha256,
            bytes,
        }
    }

    /// Verify the bytes hash to the recorded digest.
    pub fn verify(&self) -> bool {
        *blake3::hash(&self.bytes).as_bytes() == self.sha256
    }
}

/// Write a casefile.
pub fn write_casefile(path: &Path, entries: &[CaseEntry]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&CASEFILE_MAGIC)?;
    f.write_all(&[CASEFILE_VERSION])?;
    f.write_all(&(entries.len() as u32).to_le_bytes())?;
    for e in entries {
        let name_len = u16::try_from(e.name.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "name too long"))?;
        f.write_all(&name_len.to_le_bytes())?;
        f.write_all(&e.name)?;
        f.write_all(&e.sha256)?;
        f.write_all(&(e.bytes.len() as u64).to_le_bytes())?;
        f.write_all(&e.bytes)?;
    }
    f.sync_all()?;
    Ok(())
}

/// Errors from casefile parsing (never panics on corrupt input).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasefileError {
    /// I/O failure.
    Io(String),
    /// Bad magic/version.
    Malformed,
    /// Truncated file.
    Truncated,
    /// Entry digest mismatch (bytes corrupted or wrong file).
    HashMismatch(String),
}

impl std::fmt::Display for CasefileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CasefileError {}

impl From<std::io::Error> for CasefileError {
    fn from(e: std::io::Error) -> Self {
        CasefileError::Io(e.to_string())
    }
}

/// Read and verify a casefile.
pub fn read_casefile(path: &Path) -> Result<Vec<CaseEntry>, CasefileError> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    parse_casefile(&buf)
}

/// Parse casefile bytes (independent of the file, for fuzzing).
pub fn parse_casefile(buf: &[u8]) -> Result<Vec<CaseEntry>, CasefileError> {
    if buf.len() < 5 || buf[..4] != CASEFILE_MAGIC {
        return Err(CasefileError::Malformed);
    }
    if buf[4] != CASEFILE_VERSION {
        return Err(CasefileError::Malformed);
    }
    let count = u32::from_le_bytes(buf[5..9].try_into().map_err(|_| CasefileError::Truncated)?);
    let mut pos = 9usize;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if pos + 2 > buf.len() {
            return Err(CasefileError::Truncated);
        }
        let name_len = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        if pos + name_len + 32 + 8 > buf.len() {
            return Err(CasefileError::Truncated);
        }
        let name = buf[pos..pos + name_len].to_vec();
        pos += name_len;
        let sha256: [u8; 32] = buf[pos..pos + 32].try_into().unwrap();
        pos += 32;
        let bytes_len = u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let bytes_len = usize::try_from(bytes_len).map_err(|_| CasefileError::Malformed)?;
        if pos + bytes_len > buf.len() {
            return Err(CasefileError::Truncated);
        }
        let bytes = buf[pos..pos + bytes_len].to_vec();
        pos += bytes_len;
        let entry = CaseEntry {
            name,
            sha256,
            bytes,
        };
        if !entry.verify() {
            return Err(CasefileError::HashMismatch(
                String::from_utf8_lossy(&entry.name).into_owned(),
            ));
        }
        out.push(entry);
    }
    if pos != buf.len() {
        return Err(CasefileError::Malformed);
    }
    Ok(out)
}

/// Write casefile bytes atomically (temp + rename).
pub fn write_casefile_atomic(path: &Path, entries: &[CaseEntry]) -> Result<(), CasefileError> {
    let mut buf = Vec::new();
    {
        let mut sink = std::io::Cursor::new(&mut buf);
        sink.write_all(&CASEFILE_MAGIC)?;
        sink.write_all(&[CASEFILE_VERSION])?;
        sink.write_all(&(entries.len() as u32).to_le_bytes())?;
        for e in entries {
            let name_len = u16::try_from(e.name.len()).map_err(|_| CasefileError::Malformed)?;
            sink.write_all(&name_len.to_le_bytes())?;
            sink.write_all(&e.name)?;
            sink.write_all(&e.sha256)?;
            sink.write_all(&(e.bytes.len() as u64).to_le_bytes())?;
            sink.write_all(&e.bytes)?;
        }
    }
    crate::store::write_atomic(path, &buf).map_err(|e| CasefileError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom};

    #[test]
    fn roundtrip_and_verify() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("corpus.case");
        let entries = vec![
            CaseEntry::new("a.txt", b"hello world".to_vec()),
            CaseEntry::new("b.bin", vec![0u8; 4096]),
            CaseEntry::new("c/路径", (0..=255u8).collect()),
        ];
        write_casefile(&path, &entries).unwrap();
        let back = read_casefile(&path).unwrap();
        assert_eq!(back, entries);
        assert!(back.iter().all(|e| e.verify()));
    }

    #[test]
    fn corrupt_byte_detected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("corpus.case");
        let entries = vec![CaseEntry::new("x", b"payload".to_vec())];
        write_casefile(&path, &entries).unwrap();
        // Flip a payload byte: the digest must catch it.
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.seek(SeekFrom::End(-3)).unwrap();
        let mut b = [0u8; 1];
        f.read_exact(&mut b).unwrap();
        b[0] ^= 0x01;
        f.seek(SeekFrom::End(-3)).unwrap();
        f.write_all(&b).unwrap();
        drop(f);
        assert!(matches!(
            read_casefile(&path),
            Err(CasefileError::HashMismatch(_))
        ));
    }

    #[test]
    fn truncated_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("corpus.case");
        let entries = vec![CaseEntry::new("x", b"payload".to_vec())];
        write_casefile(&path, &entries).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        for cut in [0usize, 3, 9, bytes.len() - 1] {
            assert!(parse_casefile(&bytes[..cut]).is_err(), "cut {cut}");
        }
    }
}
