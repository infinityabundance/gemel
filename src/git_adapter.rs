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

/// A parsed Git commit (GIT_INTEROP.md §4).
#[derive(Debug, Clone)]
pub struct GitCommit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: String,
    pub committer: String,
    pub message: String,
}

/// One recursive tree entry: (mode, object kind, oid, path).
#[derive(Debug, Clone)]
pub struct GitTreeEntry {
    pub mode: u32,
    pub kind: String,
    pub oid: String,
    pub path: String,
}

/// `git rev-list --topo-order --reverse <head>`: commits oldest-first in
/// topological order (parents before children), deterministic.
pub fn rev_list_topo_reverse(root: &Path, head: &str) -> Result<Vec<String>, Error> {
    let out = git(root, &["rev-list", "--topo-order", "--reverse", head])?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.len() == 40)
        .collect())
}

/// `git cat-file commit <oid>` → parsed commit.
pub fn cat_commit(root: &Path, oid: &str) -> Result<GitCommit, Error> {
    let raw = git(root, &["cat-file", "commit", oid])?;
    parse_commit(&raw)
}

/// `git ls-tree -r -z <tree>` → recursive entries (deterministic order:
/// git's own tree order; the caller sorts by path where order matters).
pub fn ls_tree_recursive(root: &Path, tree: &str) -> Result<Vec<GitTreeEntry>, Error> {
    let out = git(root, &["ls-tree", "-r", "-z", tree])?;
    let mut entries = Vec::new();
    for rec in out.split('\0') {
        if rec.is_empty() {
            continue;
        }
        // <mode> <kind> <oid>\t<path>
        let (meta, path) = rec
            .split_once('\t')
            .ok_or_else(|| Error::Invalid(format!("malformed ls-tree record {rec:?}")))?;
        let mut parts = meta.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| Error::Invalid("ls-tree missing mode".into()))?;
        let kind = parts
            .next()
            .ok_or_else(|| Error::Invalid("ls-tree missing kind".into()))?;
        let oid = parts
            .next()
            .ok_or_else(|| Error::Invalid("ls-tree missing oid".into()))?;
        entries.push(GitTreeEntry {
            mode: u32::from_str_radix(mode, 8)
                .map_err(|_| Error::Invalid(format!("bad ls-tree mode {mode:?}")))?,
            kind: kind.to_string(),
            oid: oid.to_string(),
            path: path.to_string(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// `git cat-file blob <oid>` → content bytes.
pub fn cat_blob(root: &Path, oid: &str) -> Result<Vec<u8>, Error> {
    Ok(git(root, &["cat-file", "blob", oid])?.into_bytes())
}

/// Parses a raw commit object.
pub fn parse_commit(raw: &str) -> Result<GitCommit, Error> {
    let mut lines = raw.lines();
    let mut tree = String::new();
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut committer = String::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("author ") {
            author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("committer ") {
            committer = rest.to_string();
        }
    }
    if tree.len() != 40 {
        return Err(Error::Invalid("commit missing tree".into()));
    }
    Ok(GitCommit {
        tree,
        parents,
        author,
        committer,
        message: lines.collect::<Vec<_>>().join("\n"),
    })
}

/// The author/committer timestamp from a `name <email> ts tz` line.
pub fn person_timestamp(line: &str) -> i64 {
    line.rsplit(' ')
        .nth(1)
        .and_then(|t| t.parse().ok())
        .unwrap_or(0)
}

/// The name from a `name <email> ts tz` line.
pub fn person_name(line: &str) -> String {
    let before = line.split('<').next().unwrap_or("").trim().to_string();
    if before.is_empty() {
        "unknown".to_string()
    } else {
        before
    }
}

/// The email from a `name <email> ts tz` line.
pub fn person_email(line: &str) -> Option<String> {
    line.split('<')
        .nth(1)
        .and_then(|s| s.split('>').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `git clone <url> <dir>` (argv-safe).
pub fn clone_repo(url: &str, dir: &Path) -> Result<(), Error> {
    let out = Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(url)
        .arg(dir)
        .output()
        .map_err(|e| Error::Invalid(format!("git unavailable: {e}")))?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

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
