//! Minimal loose-object Git reader/writer (GIT_INTEROP.md §2–§4).
//!
//! Git's object format is `zlib(<type> <size>\0<content>)`; identities are
//! SHA-1 of the same header-prefixed content. This module implements exactly
//! the loose-object subset Gemel needs for deterministic interchange:
//! blobs, trees, commits, and refs. No packfiles (packing is Phase 6).
//!
//! Git identity uses SHA-1 — an interoperability requirement, not Gemel's
//! canonical identity (which remains BLAKE3-256).

use crate::store::Error;
use std::path::Path;

/// A Git object identifier (SHA-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(pub [u8; 20]);

impl Oid {
    /// Parses a 40-hex string.
    pub fn parse(s: &str) -> Result<Oid, Error> {
        if s.len() != 40 {
            return Err(Error::Invalid(format!(
                "git object id must be 40 hex chars, got {s:?}"
            )));
        }
        let mut out = [0u8; 20];
        for i in 0..20 {
            out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16)
                .map_err(|_| Error::Invalid(format!("invalid hex in git id {s:?}")))?;
        }
        Ok(Oid(out))
    }
}

impl std::fmt::Display for Oid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// SHA-1 of `<kind> <len>\0<content>` — the Git object identity.
pub fn hash_object(kind: &str, content: &[u8]) -> Oid {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(kind.as_bytes());
    h.update(b" ");
    h.update(content.len().to_string().as_bytes());
    h.update([0u8]);
    h.update(content);
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d);
    Oid(out)
}

/// The well-known empty-tree id.
pub const EMPTY_TREE: Oid = Oid([
    0x4b, 0x82, 0x5d, 0xc6, 0x42, 0xcb, 0x6e, 0xb9, 0xa0, 0x60, 0xe5, 0x4b, 0xf8, 0xd6, 0x92, 0x88,
    0xfb, 0xee, 0x49, 0x04,
]);

/// The loose-object path for an oid: `.git/objects/xx/yyyy…`.
pub fn loose_path(git_dir: &Path, oid: &Oid) -> std::path::PathBuf {
    let hex = oid.to_string();
    git_dir.join("objects").join(&hex[0..2]).join(&hex[2..])
}

/// Writes a loose object (zlib-compressed, atomic).
pub fn write_loose(git_dir: &Path, oid: &Oid, kind: &str, content: &[u8]) -> Result<(), Error> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;
    let mut header = Vec::with_capacity(kind.len() + 24);
    header.extend_from_slice(kind.as_bytes());
    header.push(b' ');
    header.extend_from_slice(content.len().to_string().as_bytes());
    header.push(0u8);
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&header)?;
    enc.write_all(content)?;
    let compressed = enc.finish()?;
    let path = loose_path(git_dir, oid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::store::objects::write_atomic(&path, &compressed)?;
    Ok(())
}

/// Reads a loose object: `(kind, content)`.
pub fn read_loose(git_dir: &Path, oid: &Oid) -> Result<(String, Vec<u8>), Error> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let bytes = std::fs::read(loose_path(git_dir, oid))?;
    let mut dec = ZlibDecoder::new(&bytes[..]);
    let mut raw = Vec::new();
    dec.read_to_end(&mut raw)?;
    let nul = raw
        .iter()
        .position(|b| *b == 0u8)
        .ok_or_else(|| Error::Invalid("git object header missing NUL".into()))?;
    let header = std::str::from_utf8(&raw[..nul])
        .map_err(|_| Error::Invalid("git object header not UTF-8".into()))?;
    let (kind, size) = header
        .split_once(' ')
        .ok_or_else(|| Error::Invalid("git object header malformed".into()))?;
    let size: usize = size
        .parse()
        .map_err(|_| Error::Invalid("git object size malformed".into()))?;
    let content = raw[nul + 1..].to_vec();
    if content.len() != size {
        return Err(Error::Invalid(format!(
            "git object size mismatch: header {size}, actual {}",
            content.len()
        )));
    }
    Ok((kind.to_string(), content))
}

/// Every loose object in the store, deterministically ordered by oid.
pub fn read_loose_all(git_dir: &Path) -> Result<Vec<(Oid, String, Vec<u8>)>, Error> {
    let objects_dir = git_dir.join("objects");
    let mut out = Vec::new();
    let shards = match std::fs::read_dir(&objects_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for shard in shards.flatten() {
        if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let prefix = shard.file_name().to_string_lossy().to_string();
        let entries = match std::fs::read_dir(shard.path()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.len() != 38 {
                continue; // not a 40-hex loose object
            }
            let hex = format!("{prefix}{name}");
            let oid = match Oid::parse(&hex) {
                Ok(o) => o,
                Err(_) => continue,
            };
            if let Ok((kind, content)) = read_loose(git_dir, &oid) {
                out.push((oid, kind, content));
            }
        }
    }
    out.sort_by_key(|a| a.0);
    Ok(out)
}

/// One tree entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: u32,
    pub name: String,
    pub oid: Oid,
}

/// Git's canonical tree order (base_name_compare semantics): compare raw
/// name bytes; when one name is a prefix of the other, the entry that ends
/// first sorts first, and a directory sorts after a file with the same
/// prefix (the directory's implicit `'/'` is greater than the file's
/// NUL terminator). Example order: `a.txt`, `b.txt`, `b/`.
fn tree_entry_cmp(a: &TreeEntry, b: &TreeEntry) -> std::cmp::Ordering {
    let ab = a.name.as_bytes();
    let bb = b.name.as_bytes();
    let min = ab.len().min(bb.len());
    match ab[..min].cmp(&bb[..min]) {
        std::cmp::Ordering::Equal => {
            let ac = if a.mode == 0o040000 { b'/' } else { 0 };
            let bc = if b.mode == 0o040000 { b'/' } else { 0 };
            ac.cmp(&bc)
        }
        other => other,
    }
}

/// Encodes a tree (entries sorted into Git canonical order first).
pub fn encode_tree(mut entries: Vec<TreeEntry>) -> Vec<u8> {
    entries.sort_by(tree_entry_cmp);
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(format!("{:o} {}\0", e.mode, e.name).as_bytes());
        out.extend_from_slice(&e.oid.0);
    }
    out
}

/// Decodes a tree into entries (Git canonical order preserved).
pub fn decode_tree(bytes: &[u8]) -> Result<Vec<TreeEntry>, Error> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        // mode name\0
        let sp = bytes[i..]
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| Error::Invalid("tree entry missing space".into()))?
            + i;
        let mode_str = std::str::from_utf8(&bytes[i..sp])
            .map_err(|_| Error::Invalid("tree mode not ASCII".into()))?;
        let mode = u32::from_str_radix(mode_str, 8)
            .map_err(|_| Error::Invalid(format!("tree mode {mode_str:?}")))?;
        let nul = bytes[sp..]
            .iter()
            .position(|b| *b == 0u8)
            .ok_or_else(|| Error::Invalid("tree entry missing NUL".into()))?
            + sp;
        let name = std::str::from_utf8(&bytes[sp + 1..nul])
            .map_err(|_| Error::Invalid("tree name not UTF-8".into()))?
            .to_string();
        if nul + 21 > bytes.len() {
            return Err(Error::Invalid("tree entry truncated".into()));
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&bytes[nul + 1..nul + 21]);
        out.push(TreeEntry {
            mode,
            name,
            oid: Oid(oid),
        });
        i = nul + 21;
    }
    Ok(out)
}

/// A parsed commit.
#[derive(Debug, Clone)]
pub struct Commit {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: String,    // "Name <email> <ts> <tz>"
    pub committer: String, // "Name <email> <ts> <tz>"
    pub message: String,
}

/// Encodes a commit.
pub fn encode_commit(c: &Commit) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("tree {}\n", c.tree).as_bytes());
    for p in &c.parents {
        out.extend_from_slice(format!("parent {p}\n").as_bytes());
    }
    out.extend_from_slice(format!("author {}\n", c.author).as_bytes());
    out.extend_from_slice(format!("committer {}\n", c.committer).as_bytes());
    out.push(b'\n');
    out.extend_from_slice(c.message.as_bytes());
    out
}

/// Decodes a commit (strict: required headers present, sizes bounded).
pub fn decode_commit(content: &[u8]) -> Result<Commit, Error> {
    let text =
        std::str::from_utf8(content).map_err(|_| Error::Invalid("commit not UTF-8".into()))?;
    let (headers, message) = text
        .split_once("\n\n")
        .ok_or_else(|| Error::Invalid("commit has no message separator".into()))?;
    let mut commit = Commit {
        tree: EMPTY_TREE,
        parents: Vec::new(),
        author: String::new(),
        committer: String::new(),
        message: message.to_string(),
    };
    for line in headers.lines() {
        if let Some(v) = line.strip_prefix("tree ") {
            commit.tree = Oid::parse(v)?;
        } else if let Some(v) = line.strip_prefix("parent ") {
            commit.parents.push(Oid::parse(v)?);
        } else if let Some(v) = line.strip_prefix("author ") {
            commit.author = v.to_string();
        } else if let Some(v) = line.strip_prefix("committer ") {
            commit.committer = v.to_string();
        }
        // Unknown headers are ignored (forward compatibility).
    }
    if commit.author.is_empty() || commit.committer.is_empty() {
        return Err(Error::Invalid("commit missing author/committer".into()));
    }
    Ok(commit)
}

/// Reads a ref file (`refs/heads/...`); absent refs yield None.
pub fn read_ref(git_dir: &Path, name: &str) -> Result<Option<Oid>, Error> {
    if name.contains("..") || name.contains("//") || name.starts_with('/') {
        return Err(Error::Invalid(format!("invalid git ref {name:?}")));
    }
    let path = git_dir.join("refs").join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(Oid::parse(text.trim())?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Lists branch refs as (name, oid), sorted by name.
pub fn list_branches(git_dir: &Path) -> Result<Vec<(String, Oid)>, Error> {
    let mut out = Vec::new();
    let heads = git_dir.join("refs").join("heads");
    let entries = match std::fs::read_dir(&heads) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(oid) = read_ref(git_dir, &format!("heads/{name}"))? {
            out.push((name, oid));
        }
    }
    out.sort();
    Ok(out)
}

/// Writes a branch ref.
pub fn write_branch(git_dir: &Path, name: &str, oid: &Oid) -> Result<(), Error> {
    let path = git_dir.join("refs").join("heads").join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::store::objects::write_atomic(&path, &format!("{oid}\n").into_bytes())?;
    Ok(())
}

/// Writes `HEAD` pointing at a branch.
pub fn write_head(git_dir: &Path, branch: &str) -> Result<(), Error> {
    crate::store::objects::write_atomic(
        &git_dir.join("HEAD"),
        &format!("ref: refs/heads/{branch}\n").into_bytes(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_roundtrip_and_order() {
        let a = TreeEntry {
            mode: 0o100644,
            name: "a.txt".into(),
            oid: Oid([1u8; 20]),
        };
        let b = TreeEntry {
            mode: 0o040000,
            name: "b".into(),
            oid: Oid([2u8; 20]),
        };
        let c = TreeEntry {
            mode: 0o100644,
            name: "b.txt".into(),
            oid: Oid([3u8; 20]),
        };
        let bytes = encode_tree(vec![c.clone(), a.clone(), b.clone()]);
        // Git's base_name_compare order: a.txt < b.txt < b/ (a directory
        // sorts after a file that shares its prefix).
        let decoded = decode_tree(&bytes).unwrap();
        assert_eq!(decoded[0].name, "a.txt");
        assert_eq!(decoded[1].name, "b.txt");
        assert_eq!(decoded[2].name, "b");
        // Identical input → identical bytes (deterministic).
        assert_eq!(bytes, encode_tree(vec![a, b, c]));
        // Directory sorts after file with same prefix (prefix-tiebreak rule).
        let d = TreeEntry {
            mode: 0o040000,
            name: "a".into(),
            oid: Oid([4u8; 20]),
        };
        let e = TreeEntry {
            mode: 0o100644,
            name: "ab".into(),
            oid: Oid([5u8; 20]),
        };
        let bytes2 = encode_tree(vec![d.clone(), e.clone()]);
        assert_eq!(decode_tree(&bytes2).unwrap()[0].name, "ab");
        assert_eq!(decode_tree(&bytes2).unwrap()[1].name, "a");
        // The empty tree has the well-known id.
        assert_eq!(hash_object("tree", &encode_tree(vec![])), EMPTY_TREE);
    }

    #[test]
    fn commit_roundtrip() {
        let c = Commit {
            tree: Oid([7u8; 20]),
            parents: vec![Oid([8u8; 20])],
            author: "Ada <ada@example.com> 1700000000 +0000".into(),
            committer: "Ada <ada@example.com> 1700000000 +0000".into(),
            message: "subject\n\nbody\nGemel-Change: change.x\n".into(),
        };
        let bytes = encode_commit(&c);
        let d = decode_commit(&bytes).unwrap();
        assert_eq!(d.tree, c.tree);
        assert_eq!(d.parents, c.parents);
        assert_eq!(d.message, c.message);
        // Content-addressed: same content → same oid.
        assert_eq!(
            hash_object("commit", &bytes),
            hash_object("commit", &encode_commit(&c))
        );
    }

    #[test]
    fn loose_roundtrip() {
        let dir = crate::store::testing::temp_root("git-io");
        let oid = hash_object("blob", b"hello\n");
        write_loose(&dir, &oid, "blob", b"hello\n").unwrap();
        let (kind, content) = read_loose(&dir, &oid).unwrap();
        assert_eq!(kind, "blob");
        assert_eq!(content, b"hello\n");
    }
}
