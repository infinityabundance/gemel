//! A narrowly isolated Git adapter (EXCHANGE.md §23, GIT_INTEROP.md).
//!
//! Shells out to the installed `git` executable for index/tree reads. Every
//! invocation is argument-safe (argv, never a shell), bounded, failure-
//! checked, and isolated from canonical Gemel semantics. The boundary is
//! replaceable by a native adapter (e.g. `gix`) later.

use crate::store::Error;
use std::path::Path;
use std::process::Command;

/// Runs `git <args>` in `cwd`; returns stdout. Never touches a shell.
fn git(cwd: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| Error::Invalid(format!("git unavailable: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Invalid(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One staged entry: (path, gemel mode, blob content).
pub type StagedEntry = (String, u64, Vec<u8>);

/// Reads the staged (index) tree: `git ls-files --stage` + `git cat-file`.
/// The Git index is an interchange surface, never part of Gemel's ontology
/// (EXCHANGE.md §23).
pub fn staged_files(root: &Path) -> Result<Vec<StagedEntry>, Error> {
    let ls = git(root, &["ls-files", "--stage"])?;
    let mut out = Vec::new();
    for line in ls.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let meta = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").to_string();
        let mut meta_parts = meta.split_whitespace();
        let mode_str = meta_parts.next().unwrap_or("");
        let oid = meta_parts.next().unwrap_or("");
        let mode = u32::from_str_radix(mode_str, 8)
            .map_err(|_| Error::Invalid(format!("malformed git mode {mode_str:?}")))?;
        if oid.len() != 40 {
            return Err(Error::Invalid(format!("malformed git blob id {oid:?}")));
        }
        if path.is_empty() || path.contains("..") {
            return Err(Error::Invalid(format!("malformed staged path {path:?}")));
        }
        let content = git(root, &["cat-file", "blob", oid])?.into_bytes();
        let gemel_mode = match mode {
            0o100644 => 0o100644,
            0o100755 => 0o100755,
            0o120000 => 0o120000,
            other => return Err(Error::Invalid(format!("unsupported staged mode {other:o}"))),
        };
        out.push((path, gemel_mode, content));
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_index_staging_roundtrip() {
        let root = crate::store::testing::temp_root("git-adapter");
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q"]);
        std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
        run(&["add", "a.txt"]);
        let files = staged_files(&root).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "a.txt");
        assert_eq!(files[0].2, b"alpha\n");
    }
}
