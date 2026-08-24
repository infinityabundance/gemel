//! A .gitignore matcher implementing a documented subset of git semantics
//! (STORAGE.md §6, content snapshotting).
//!
//! Supported pattern features: `#` comments, `!` negation (last match wins),
//! trailing `/` directory-only patterns, leading/middle/trailing `**`,
//! `*` and `?` within a segment, and anchoring (a pattern containing `/` is
//! anchored to the repository root; otherwise it matches at any depth).
//! A file cannot be re-included when an ancestor directory is excluded,
//! matching git. Nested .gitignore files are not consulted in Phase 1.

use std::path::Path;

/// The compiled ignore rules.
#[derive(Debug, Default)]
pub struct Ignore {
    rules: Vec<Rule>,
}

#[derive(Debug)]
struct Rule {
    negated: bool,
    dir_only: bool,
    anchored: bool,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// A literal segment name.
    Literal(String),
    /// A segment containing `*`/`?` wildcards.
    Wild(String),
    /// `**` — matches zero or more segments.
    DoubleStar,
}

impl Ignore {
    /// Compiles the rules from `.gitignore` text.
    pub fn parse(text: &str) -> Ignore {
        let mut rules = Vec::new();
        for raw in text.lines() {
            let mut line = raw.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut negated = false;
            if let Some(rest) = line.strip_prefix('!') {
                negated = true;
                line = rest;
            }
            if line.is_empty() {
                continue;
            }
            let mut dir_only = false;
            if let Some(rest) = line.strip_suffix('/') {
                dir_only = true;
                line = rest;
            }
            let mut anchored = false;
            if let Some(rest) = line.strip_prefix('/') {
                anchored = true;
                line = rest;
            }
            if line.contains('/') {
                anchored = true;
            }
            if let Some(pat) = parse_pattern(line) {
                rules.push(Rule {
                    negated,
                    dir_only,
                    anchored,
                    parts: pat,
                });
            }
        }
        Ignore { rules }
    }

    /// Loads rules from a `.gitignore` file at `root/.gitignore` if present.
    pub fn from_root(root: &Path) -> Ignore {
        match std::fs::read_to_string(root.join(".gitignore")) {
            Ok(text) => Ignore::parse(&text),
            Err(_) => Ignore::default(),
        }
    }

    /// Whether `rel_path` (canonical, `/`-separated) is ignored.
    ///
    /// If the path itself is a directory, pass `is_dir = true`. A file under
    /// an ignored ancestor directory is ignored regardless of later negation,
    /// matching git.
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        if rel_path.is_empty() {
            return false;
        }
        let segments: Vec<&str> = rel_path.split('/').collect();
        // Every proper ancestor prefix is a directory.
        for i in 1..segments.len() {
            let prefix = segments[..i].join("/");
            if self.decide(&prefix, true) {
                return true;
            }
        }
        self.decide(rel_path, is_dir)
    }

    fn decide(&self, path: &str, is_dir: bool) -> bool {
        let segments: Vec<&str> = path.split('/').collect();
        let mut ignored = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            if rule_matches(rule, &segments) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}

fn rule_matches(rule: &Rule, segments: &[&str]) -> bool {
    if rule.anchored {
        parts_match(&rule.parts, segments)
    } else {
        // Match at any depth: try every suffix.
        for start in 0..segments.len() {
            if parts_match(&rule.parts, &segments[start..]) {
                return true;
            }
        }
        false
    }
}

fn parts_match(parts: &[Part], segments: &[&str]) -> bool {
    match parts.first() {
        None => segments.is_empty(),
        Some(Part::DoubleStar) => {
            // `**` consumes zero or more segments.
            if parts.len() == 1 {
                return true;
            }
            for consumed in 0..=segments.len() {
                if parts_match(&parts[1..], &segments[consumed..]) {
                    return true;
                }
            }
            false
        }
        Some(Part::Literal(name)) => {
            if segments.is_empty() {
                return false;
            }
            segments[0] == name && parts_match(&parts[1..], &segments[1..])
        }
        Some(Part::Wild(pattern)) => {
            if segments.is_empty() {
                return false;
            }
            seg_match(pattern, segments[0]) && parts_match(&parts[1..], &segments[1..])
        }
    }
}

/// Wildcard match of a pattern against a single segment (`*` = any chars,
/// `?` = one char).
fn seg_match(pattern: &str, seg: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = seg.chars().collect();
    seg_match_rec(&p, &s)
}

fn seg_match_rec(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') => {
            // `*` consumes zero or more chars; try all splits.
            for i in 0..=s.len() {
                if seg_match_rec(&p[1..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        Some('?') => !s.is_empty() && seg_match_rec(&p[1..], &s[1..]),
        Some(c) => !s.is_empty() && s[0] == *c && seg_match_rec(&p[1..], &s[1..]),
    }
}

/// Splits a pattern into parts; returns None for unsupported constructs.
fn parse_pattern(pattern: &str) -> Option<Vec<Part>> {
    let mut parts = Vec::new();
    for seg in pattern.split('/') {
        if seg == "**" {
            parts.push(Part::DoubleStar);
        } else if seg.contains('*') || seg.contains('?') {
            parts.push(Part::Wild(seg.to_string()));
        } else {
            parts.push(Part::Literal(seg.to_string()));
        }
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignored(rules: &str, path: &str, is_dir: bool) -> bool {
        Ignore::parse(rules).is_ignored(path, is_dir)
    }

    #[test]
    fn simple_rules() {
        assert!(ignored("*.log\n", "a.log", false));
        assert!(!ignored("*.log\n", "a.txt", false));
        assert!(ignored("*.log\n", "deep/nested/a.log", false));
        assert!(ignored("build/\n", "build", true));
        assert!(!ignored("build/\n", "build", false));
        assert!(ignored("build/\n", "a/build", true));
    }

    #[test]
    fn anchored_patterns() {
        // `/foo` is anchored; `foo` matches anywhere.
        assert!(ignored("/foo\n", "foo", false));
        assert!(!ignored("/foo\n", "a/foo", false));
        assert!(ignored("foo\n", "a/foo", false));
        // Middle slash anchors too.
        assert!(ignored("a/b\n", "a/b", false));
        assert!(!ignored("a/b\n", "x/a/b", false));
    }

    #[test]
    fn double_star() {
        assert!(ignored("**/foo\n", "a/b/foo", false));
        assert!(ignored("**/foo\n", "foo", false));
        assert!(ignored("foo/**\n", "foo/x/y", false));
        assert!(!ignored("foo/**\n", "x/foo", false));
    }

    #[test]
    fn negation_last_match_wins() {
        let rules = "*.log\n!keep.log\n";
        assert!(!ignored(rules, "keep.log", false));
        assert!(ignored(rules, "drop.log", false));
    }

    #[test]
    fn parent_dir_exclusion_wins() {
        // Git: cannot re-include files under an excluded directory.
        let rules = "build/\n!build/keep.txt\n";
        assert!(ignored(rules, "build/keep.txt", false));
    }

    #[test]
    fn question_mark_and_comment() {
        assert!(ignored("file?.txt\n", "file1.txt", false));
        assert!(!ignored("file?.txt\n", "file10.txt", false));
        assert!(!ignored("# comment\n", "anything", false));
    }
}
