//! Deterministic Rust item extraction (Phase 5).
//!
//! A lexical scanner — not a full parser — that extracts *declarations* with
//! exact spans from Rust source, correctly skipping strings, raw strings,
//! char literals, line/block comments, and attributes. It never misreads
//! `fn` inside a string literal or a comment. Extraction is deterministic:
//! identical bytes produce identical entities.
//!
//! Language intelligence is an enhancement, never a storage dependency:
//! arbitrary files (and non-Rust projects) work untouched without this module.

use crate::store::Error;

/// The kinds a scanner can emit (a subset of the canonical entity kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScanKind {
    Module,
    Type,
    Trait,
    Impl,
    Function,
    Method,
    Constant,
    Static,
    Test,
    Other,
}

impl ScanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanKind::Module => "module",
            ScanKind::Type => "type",
            ScanKind::Trait => "trait",
            ScanKind::Impl => "impl",
            ScanKind::Function => "function",
            ScanKind::Method => "method",
            ScanKind::Constant => "constant",
            ScanKind::Static => "static",
            ScanKind::Test => "test",
            ScanKind::Other => "other",
        }
    }
}

/// One extracted item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedItem {
    pub kind: ScanKind,
    pub name: String,
    /// The module path of the containing module (e.g. `crate::parser`).
    pub module_path: String,
    pub start_line: u64,
    pub end_line: u64,
    /// The item header (keyword through the body open or `;`), whitespace-
    /// collapsed, capped.
    pub signature: String,
    /// `public` / `crate` / `private`.
    pub visibility: String,
    /// The `impl` target type name (for impls).
    pub impl_target: Option<String>,
    /// The trait name for `impl Trait for Type`.
    pub impl_trait: Option<String>,
}

/// A `use` declaration at module level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseDecl {
    pub path: String,
    pub line: u64,
}

/// The scan result of one file.
#[derive(Debug, Clone)]
pub struct FileScan {
    pub file_path: String,
    /// The module path derived from the file location.
    pub file_module_path: String,
    pub items: Vec<ExtractedItem>,
    pub uses: Vec<UseDecl>,
}

/// Derives the module path of a `.rs` file from its repository path
/// (`src/lib.rs` → `crate`, `src/dns/name/parser.rs` → `crate::dns::name::parser`).
pub fn module_path_for_file(file_path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in file_path.split('/') {
        if seg.is_empty() || seg == "src" {
            continue;
        }
        if seg == "lib.rs" || seg == "main.rs" {
            continue; // crate roots
        }
        if seg == "mod.rs" {
            continue; // the parent dir carries the module name
        }
        // Directory segments carry module names; file segments contribute
        // their stem.
        if let Some(stem) = seg.strip_suffix(".rs") {
            parts.push(stem);
        } else {
            parts.push(seg);
        }
    }
    if parts.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", parts.join("::"))
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    line: u64,
    line_start: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor {
            bytes,
            pos: 0,
            line: 1,
            line_start: 0,
        }
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn peek_at(&self, off: usize) -> Option<u8> {
        self.bytes.get(self.pos + off).copied()
    }
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.line_start = self.pos;
        }
        Some(b)
    }
}

/// True when the cursor is at the start of a raw string literal: `r"`,
/// `r#"`, ... or a byte raw string `br"`, `br#"`, ... . The `b` prefix,
/// `r`, any `#`s and the opening quote are all required.
fn at_raw_string(c: &Cursor) -> bool {
    let mut i: usize;
    if c.peek_at(0) == Some(b'b') && c.peek_at(1) == Some(b'r') {
        i = 2;
    } else if c.peek_at(0) == Some(b'r') {
        i = 1;
    } else {
        return false;
    }
    while c.peek_at(i) == Some(b'#') {
        i += 1;
    }
    c.peek_at(i) == Some(b'"')
}

/// Scans one file. `file_path` is the repository-relative path.
pub fn scan_file(file_path: &str, content: &[u8]) -> Result<FileScan, Error> {
    let module = module_path_for_file(file_path);
    let mut c = Cursor::new(content);
    let mut items: Vec<ExtractedItem> = Vec::new();
    let mut uses: Vec<UseDecl> = Vec::new();
    let mut pending_test = false;
    let mut pending_vis: String = "private".to_string();
    // The keyword start of a `pub`-qualified item, carried from the `pub`
    // token to the item it qualifies so signatures include the qualifier.
    let mut pending_kw_start: Option<usize> = None;
    let mut module_stack: Vec<String> = Vec::new();
    let mut mod_depths: Vec<i64> = Vec::new(); // brace depth at module open
    let mut brace_depth: i64 = 0;

    while let Some(b) = c.peek() {
        match b {
            b'/' if c.peek_at(1) == Some(b'/') => skip_line_comment(&mut c),
            b'/' if c.peek_at(1) == Some(b'*') => skip_block_comment(&mut c),
            b'"' => skip_string(&mut c)?,
            _ if at_raw_string(&c) => {
                if c.peek() == Some(b'b') {
                    c.bump();
                }
                skip_raw_string(&mut c)?;
            }
            b'\'' => skip_char_or_lifetime(&mut c),
            b'#' if c.peek_at(1) == Some(b'[') => {
                // Attribute (outer `#[...]` or inner `#![...]`).
                c.bump();
                if c.peek() == Some(b'!') {
                    c.bump();
                }
                let start = c.pos;
                skip_balanced(&mut c, b'[', b']')?;
                let attr = std::str::from_utf8(&c.bytes[start + 1..c.pos.saturating_sub(1)])
                    .unwrap_or("")
                    .trim();
                if attr == "test" || attr.starts_with("test(") {
                    pending_test = true;
                }
            }
            _ if b.is_ascii_alphabetic() => {
                let start = c.pos;
                let word = read_word(&mut c);
                match word.as_str() {
                    "pub" => {
                        pending_kw_start = Some(start);
                        pending_vis = if c.peek() == Some(b'(') {
                            // pub(crate) / pub(super) / pub(in path)
                            skip_balanced(&mut c, b'(', b')')?;
                            "crate".to_string()
                        } else {
                            "public".to_string()
                        };
                    }
                    "fn" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let kind = if pending_test {
                                ScanKind::Test
                            } else {
                                ScanKind::Function
                            };
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                kind,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_test = false;
                        pending_vis = "private".to_string();
                    }
                    "struct" | "enum" | "union" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Type,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "trait" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Trait,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "type" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Type,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "impl" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        let (target, trait_name) = read_impl_target(&mut c)?;
                        let name = trait_name
                            .clone()
                            .map(|t| format!("{t} for {target}"))
                            .unwrap_or_else(|| target.clone());
                        let mp = module_path_for_scan(&module, &module_stack);
                        let item = finish_item(
                            &mut c,
                            content,
                            ScanKind::Impl,
                            &name,
                            &mp,
                            file_path,
                            &pending_vis,
                            Some(target),
                            trait_name,
                            kw_start,
                        )?;
                        items.push(item);
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "mod" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            skip_ws(&mut c);
                            if c.peek() == Some(b'{') {
                                // Inline module: consume only the header and
                                // the opening brace; the main loop continues
                                // inside the body so nested items are seen.
                                let start_line = line_at_pos(content, kw_start);
                                let sig = collapse_ws(&content[kw_start..c.pos], 512);
                                c.bump(); // consume `{`
                                brace_depth += 1;
                                items.push(ExtractedItem {
                                    kind: ScanKind::Module,
                                    name,
                                    module_path: mp,
                                    start_line,
                                    end_line: 0, // fixed when the module closes
                                    signature: sig,
                                    visibility: pending_vis.clone(),
                                    impl_target: None,
                                    impl_trait: None,
                                });
                                module_stack.push(items.last().unwrap().name.clone());
                                mod_depths.push(brace_depth);
                            } else {
                                // `mod x;` — file-backed module declaration.
                                let item = finish_item(
                                    &mut c,
                                    content,
                                    ScanKind::Module,
                                    &name,
                                    &mp,
                                    file_path,
                                    &pending_vis,
                                    None,
                                    None,
                                    kw_start,
                                )?;
                                items.push(item);
                            }
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "const" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Constant,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "static" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Static,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "use" => {
                        let line = c.line;
                        let path = read_use_path(&mut c);
                        if !path.is_empty() {
                            uses.push(UseDecl { path, line });
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                        pending_kw_start = None;
                    }
                    "macro_rules" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if c.peek() == Some(b'!') {
                            c.bump();
                        }
                        if let Some(name) = read_item_name(&mut c) {
                            let mp = module_path_for_scan(&module, &module_stack);
                            let item = finish_item(
                                &mut c,
                                content,
                                ScanKind::Other,
                                &name,
                                &mp,
                                file_path,
                                &pending_vis,
                                None,
                                None,
                                kw_start,
                            )?;
                            items.push(item);
                        }
                        pending_vis = "private".to_string();
                        pending_test = false;
                    }
                    "extern" => {
                        let kw_start = pending_kw_start.take().unwrap_or(start);
                        if c.peek() == Some(b'"') {
                            skip_string(&mut c)?;
                            let save = c.pos;
                            let w = read_word(&mut c);
                            if w == "fn" {
                                if let Some(name) = read_item_name(&mut c) {
                                    let mp = module_path_for_scan(&module, &module_stack);
                                    let item = finish_item(
                                        &mut c,
                                        content,
                                        ScanKind::Function,
                                        &name,
                                        &mp,
                                        file_path,
                                        &pending_vis,
                                        None,
                                        None,
                                        kw_start,
                                    )?;
                                    items.push(item);
                                }
                                pending_vis = "private".to_string();
                                pending_test = false;
                            } else {
                                c.pos = save;
                            }
                        } else {
                            let _ = read_word(&mut c);
                        }
                    }
                    _ => {
                        c.pos = start;
                        pending_kw_start = None;
                        c.bump();
                    }
                }
            }
            b'}' => {
                brace_depth -= 1;
                // Close nested inline modules whose closing brace was just
                // consumed; fix their end_line now that it is known.
                while let Some(depth) = mod_depths.last().copied() {
                    if brace_depth < depth {
                        if let Some(m) = items
                            .iter_mut()
                            .rev()
                            .find(|i| i.kind == ScanKind::Module && i.end_line == 0)
                        {
                            m.end_line = c.line;
                        }
                        module_stack.pop();
                        mod_depths.pop();
                    } else {
                        break;
                    }
                }
                c.bump();
            }
            b'{' => {
                brace_depth += 1;
                c.bump();
            }
            b';' => {
                pending_vis = "private".to_string();
                pending_test = false;
                pending_kw_start = None;
                c.bump();
            }
            _ => {
                c.bump();
            }
        }
    }
    Ok(FileScan {
        file_path: file_path.to_string(),
        file_module_path: module,
        items,
        uses,
    })
}

/// The current module path: file module + nested inline modules.
fn module_path_for_scan(base: &str, stack: &[String]) -> String {
    if stack.is_empty() {
        base.to_string()
    } else {
        format!("{base}::{}", stack.join("::"))
    }
}

/// Reads a word (identifier-ish) at the cursor, consuming it.
fn read_word(c: &mut Cursor) -> String {
    let start = c.pos;
    while let Some(b) = c.peek() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            c.bump();
        } else {
            break;
        }
    }
    String::from_utf8_lossy(&c.bytes[start..c.pos]).into_owned()
}

/// Peeks a word without consuming it.
fn peek_word(c: &mut Cursor) -> String {
    let save = (c.pos, c.line, c.line_start);
    let w = read_word(c);
    c.pos = save.0;
    c.line = save.1;
    c.line_start = save.2;
    w
}

/// After `fn`/`struct`/etc.: reads the item name (an identifier).
fn read_item_name(c: &mut Cursor) -> Option<String> {
    skip_ws(c);
    if c.peek() == Some(b'!') {
        return None; // macro invocation
    }
    let start = c.pos;
    while let Some(b) = c.peek() {
        if b.is_ascii_alphanumeric() || b == b'_' {
            c.bump();
        } else {
            break;
        }
    }
    if c.pos == start {
        return None;
    }
    Some(String::from_utf8_lossy(&c.bytes[start..c.pos]).into_owned())
}

/// Reads the target of an `impl`: `impl<T> Trait for Type` or `impl Type`.
/// Returns (type_name, Option<trait_name>).
fn read_impl_target(c: &mut Cursor) -> Result<(String, Option<String>), Error> {
    skip_ws(c);
    if c.peek() == Some(b'<') {
        skip_balanced(c, b'<', b'>')?;
        skip_ws(c);
    }
    let first = read_word(c);
    if first.is_empty() {
        return Err(Error::Invalid("impl without target".into()));
    }
    let mut current = first;
    loop {
        skip_ws(c);
        if c.peek() == Some(b':') && c.peek_at(1) == Some(b':') {
            c.bump();
            c.bump();
            let seg = read_word(c);
            if seg.is_empty() {
                break;
            }
            current = format!("{current}::{seg}");
        } else if c.peek() == Some(b'<') {
            skip_balanced(c, b'<', b'>')?;
        } else {
            break;
        }
    }
    skip_ws(c);
    if peek_word(c) == "for" {
        read_word(c);
        skip_ws(c);
        let ty = read_impl_type_path(c)?;
        return Ok((ty, Some(current)));
    }
    Ok((current, None))
}

/// Reads a type path after `for`.
fn read_impl_type_path(c: &mut Cursor) -> Result<String, Error> {
    skip_ws(c);
    let first = read_word(c);
    if first.is_empty() {
        return Err(Error::Invalid("impl for without type".into()));
    }
    let mut current = first;
    loop {
        skip_ws(c);
        if c.peek() == Some(b':') && c.peek_at(1) == Some(b':') {
            c.bump();
            c.bump();
            let seg = read_word(c);
            if seg.is_empty() {
                break;
            }
            current = format!("{current}::{seg}");
        } else if c.peek() == Some(b'<') {
            skip_balanced(c, b'<', b'>')?;
        } else {
            break;
        }
    }
    Ok(current)
}

/// After the item name: finds the body open `{` (tracking paren/bracket
/// depth) or the terminating `;`, then builds the item with the exact span
/// and the collapsed signature header.
#[allow(clippy::too_many_arguments)]
fn finish_item(
    c: &mut Cursor,
    content: &[u8],
    kind: ScanKind,
    name: &str,
    module_path: &str,
    _file_path: &str,
    visibility: &str,
    impl_target: Option<String>,
    impl_trait: Option<String>,
    keyword_start: usize,
) -> Result<ExtractedItem, Error> {
    let start_line = line_at_pos(content, keyword_start);
    // Scan forward to the body open or the terminating `;` (at depth 0).
    let mut depth = 0i64;
    let mut body_open: Option<usize> = None;
    let mut terminator: Option<usize> = None;
    let mut probe = Cursor {
        bytes: content,
        pos: c.pos,
        line: c.line,
        line_start: c.line_start,
    };
    while let Some(b) = probe.peek() {
        match b {
            b'/' if probe.peek_at(1) == Some(b'/') => skip_line_comment(&mut probe),
            b'/' if probe.peek_at(1) == Some(b'*') => skip_block_comment(&mut probe),
            b'"' => skip_string(&mut probe)?,
            _ if at_raw_string(&probe) => {
                if probe.peek() == Some(b'b') {
                    probe.bump();
                }
                skip_raw_string(&mut probe)?
            }
            b'\'' => skip_char_or_lifetime(&mut probe),
            b'(' | b'[' => {
                depth += 1;
                probe.bump();
            }
            b')' | b']' => {
                depth -= 1;
                probe.bump();
            }
            b'{' if depth == 0 => {
                body_open = Some(probe.pos);
                break;
            }
            b';' if depth == 0 => {
                terminator = Some(probe.pos);
                break;
            }
            _ => {
                probe.bump();
            }
        }
    }
    let (end_line, header_end, body_closed) = match body_open {
        Some(open) => {
            let mut bdepth = 1i64;
            let mut body_end: Option<usize> = None;
            let mut q = Cursor {
                bytes: content,
                pos: open + 1,
                line: probe.line,
                line_start: probe.line_start,
            };
            while let Some(b) = q.peek() {
                match b {
                    b'/' if q.peek_at(1) == Some(b'/') => skip_line_comment(&mut q),
                    b'/' if q.peek_at(1) == Some(b'*') => skip_block_comment(&mut q),
                    b'"' => skip_string(&mut q)?,
                    _ if at_raw_string(&q) => {
                        if q.peek() == Some(b'b') {
                            q.bump();
                        }
                        skip_raw_string(&mut q)?
                    }
                    b'\'' => skip_char_or_lifetime(&mut q),
                    b'{' => {
                        bdepth += 1;
                        q.bump();
                    }
                    b'}' => {
                        bdepth -= 1;
                        q.bump();
                        if bdepth == 0 {
                            body_end = Some(q.pos);
                            break;
                        }
                    }
                    _ => {
                        q.bump();
                    }
                }
            }
            let end_line = match body_end {
                Some(e) => line_at_pos(content, e.saturating_sub(1)),
                None => line_at_pos(content, open), // truncated body
            };
            (end_line, open, body_end)
        }
        None => (
            line_at_pos(content, terminator.unwrap_or(probe.pos)),
            terminator.unwrap_or(probe.pos),
            None,
        ),
    };
    // Advance the outer cursor past the whole item: past the closing `}` of
    // a body, past the terminating `;`, or to EOF for a truncated body.
    let consume_to = match (body_open, body_closed) {
        (Some(_), Some(end)) => end,      // position past the closing `}`
        (Some(_), None) => content.len(), // truncated: give up
        (None, _) => terminator.map(|t| t + 1).unwrap_or(probe.pos),
    };
    c.pos = consume_to;
    c.line = line_at_pos(content, c.pos);
    c.line_start = content[..c.pos]
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    // Signature: keyword..header_end, whitespace-collapsed, capped.
    let header = &content[keyword_start..header_end.min(content.len())];
    let signature = collapse_ws(header, 512);
    Ok(ExtractedItem {
        kind,
        name: name.to_string(),
        module_path: module_path.to_string(),
        start_line,
        end_line,
        signature,
        visibility: visibility.to_string(),
        impl_target,
        impl_trait,
    })
}

fn line_at_pos(content: &[u8], pos: usize) -> u64 {
    content[..pos.min(content.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u64
        + 1
}

/// Collapses whitespace (including newlines) into single spaces, trimming,
/// and caps the result.
fn collapse_ws(bytes: &[u8], cap: usize) -> String {
    let mut out = String::with_capacity(bytes.len().min(cap));
    let mut prev_space = false;
    for &b in bytes {
        if b == b'\n' || b == b'\r' || b == b'\t' || b == b' ' {
            prev_space = !out.is_empty();
            continue;
        }
        if prev_space && !out.is_empty() {
            out.push(' ');
        }
        prev_space = false;
        out.push(b as char);
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// Reads the first path segment(s) of a `use` declaration up to a group
/// separator.
fn read_use_path(c: &mut Cursor) -> String {
    skip_ws(c);
    let mut out = String::new();
    loop {
        let seg = read_word(c);
        if seg.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push_str("::");
        }
        out.push_str(&seg);
        skip_ws(c);
        if c.peek() == Some(b':') && c.peek_at(1) == Some(b':') {
            c.bump();
            c.bump();
            skip_ws(c);
            continue;
        }
        if matches!(
            c.peek(),
            Some(b'{') | Some(b',') | Some(b';') | Some(b'*') | None
        ) {
            break;
        }
        if peek_word(c) == "as" {
            break;
        }
        break;
    }
    out
}

fn skip_ws(c: &mut Cursor) {
    while let Some(b) = c.peek() {
        if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
            c.bump();
        } else {
            break;
        }
    }
}

fn skip_line_comment(c: &mut Cursor) {
    while let Some(b) = c.bump() {
        if b == b'\n' {
            break;
        }
    }
}

fn skip_block_comment(c: &mut Cursor) {
    c.bump(); // /
    c.bump(); // *
    let mut depth = 1i64;
    while let Some(b) = c.peek() {
        if b == b'/' && c.peek_at(1) == Some(b'*') {
            depth += 1;
            c.bump();
            c.bump();
        } else if b == b'*' && c.peek_at(1) == Some(b'/') {
            depth -= 1;
            c.bump();
            c.bump();
            if depth == 0 {
                break;
            }
        } else {
            c.bump();
        }
    }
}

fn skip_string(c: &mut Cursor) -> Result<(), Error> {
    c.bump(); // opening quote
    while let Some(b) = c.bump() {
        if b == b'\\' {
            c.bump();
        } else if b == b'"' || b == b'\n' {
            break;
        }
    }
    Ok(())
}

fn skip_raw_string(c: &mut Cursor) -> Result<(), Error> {
    c.bump(); // r
    let mut hashes = 0;
    while c.peek() == Some(b'#') {
        hashes += 1;
        c.bump();
    }
    if c.peek() != Some(b'"') {
        return Ok(());
    }
    c.bump(); // "
              // The terminator is a `"` followed by exactly `hashes` `#`s.
    while let Some(b) = c.bump() {
        if b != b'"' {
            continue;
        }
        let mut ok = true;
        for _ in 0..hashes {
            if c.peek() == Some(b'#') {
                c.bump();
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            break;
        }
    }
    Ok(())
}

fn skip_char_or_lifetime(c: &mut Cursor) {
    c.bump(); // '
    match c.peek() {
        Some(b'\\') => {
            c.bump();
            c.bump();
            if c.peek() == Some(b'\'') {
                c.bump();
            }
        }
        Some(b) if b.is_ascii_alphanumeric() || b == b'_' => {
            c.bump();
            if c.peek() == Some(b'\'') {
                c.bump(); // char literal
            }
        }
        _ => {}
    }
}

/// Skips a balanced group. The cursor must be at the opener; it is consumed.
fn skip_balanced(c: &mut Cursor, open: u8, close: u8) -> Result<(), Error> {
    if c.peek() != Some(open) {
        return Err(Error::Invalid(format!(
            "expected opener {} at byte {}",
            open as char, c.pos
        )));
    }
    c.bump(); // consume the opener
    let mut depth = 1i64;
    while let Some(b) = c.peek() {
        match b {
            b'/' if c.peek_at(1) == Some(b'/') => skip_line_comment(c),
            b'/' if c.peek_at(1) == Some(b'*') => skip_block_comment(c),
            b'"' => skip_string(c)?,
            _ if at_raw_string(c) => {
                if c.peek() == Some(b'b') {
                    c.bump();
                }
                skip_raw_string(c)?
            }
            b'\'' => skip_char_or_lifetime(c),
            x if x == open => {
                depth += 1;
                c.bump();
            }
            x if x == close => {
                depth -= 1;
                c.bump();
                if depth == 0 {
                    break;
                }
            }
            _ => {
                c.bump();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(path: &str, src: &str) -> FileScan {
        scan_file(path, src.as_bytes()).unwrap()
    }

    #[test]
    fn extracts_basic_items_with_spans() {
        let src = "pub fn decode_name(data: &[u8]) -> String {\n    let x = 1;\n    x.to_string()\n}\n\nfn helper() {}\n";
        let s = scan("src/parser.rs", src);
        assert_eq!(s.items.len(), 2);
        let f = &s.items[0];
        assert_eq!(f.kind, ScanKind::Function);
        assert_eq!(f.name, "decode_name");
        assert_eq!(f.module_path, "crate::parser");
        assert_eq!(f.start_line, 1);
        assert_eq!(f.end_line, 4);
        assert_eq!(f.visibility, "public");
        assert!(f
            .signature
            .contains("pub fn decode_name(data: &[u8]) -> String"));
        let h = &s.items[1];
        assert_eq!(h.name, "helper");
        assert_eq!(h.end_line, 6);
    }

    #[test]
    fn skips_strings_comments_and_raw_strings() {
        let src = "\n// fn not_a_function() {}\n/* fn also_not() {} */\nconst MSG: &str = \"fn inside string\";\nfn real() {\n    let s = \"plain string\";\n    let c = 'f';\n    // fn still_not() {}\n    let life: &'static str = \"x\";\n    s\n}\n";
        let s = scan("src/parser.rs", src);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["MSG", "real"]);
        assert_eq!(s.items[0].kind, ScanKind::Constant);
        assert_eq!(s.items[1].end_line, 11);
    }

    #[test]
    fn raw_string_with_braces_does_not_confuse_spanning() {
        let src = "fn raw_holder() {\n    let s = r#\"fn inside raw { }\"#;\n    s\n}\n";
        let s = scan("src/parser.rs", src);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].name, "raw_holder");
        // The braces inside the raw string must not close the body early.
        assert_eq!(s.items[0].end_line, 4);
    }

    #[test]
    fn nested_modules_and_mod_rs_paths() {
        let src = "mod inner {\n    pub struct Item;\n}\npub mod outer {\n    pub fn go() {}\n}\n";
        let s = scan("src/dns/name/mod.rs", src);
        let mods: Vec<&str> = s
            .items
            .iter()
            .filter(|i| i.kind == ScanKind::Module)
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(mods, vec!["inner", "outer"]);
        let struct_item = s.items.iter().find(|i| i.name == "Item").unwrap();
        assert_eq!(struct_item.module_path, "crate::dns::name::inner");
        let go = s.items.iter().find(|i| i.name == "go").unwrap();
        assert_eq!(go.module_path, "crate::dns::name::outer");
        assert_eq!(s.file_module_path, "crate::dns::name");
    }

    #[test]
    fn lib_rs_is_crate_root() {
        let s = scan("src/lib.rs", "pub fn root_fn() {}\n");
        assert_eq!(s.file_module_path, "crate");
        assert_eq!(s.items[0].module_path, "crate");
    }

    #[test]
    fn tests_attributes_and_impls() {
        let src = "#[test]\nfn check_roundtrip() {}\n\nimpl Parser {\n    pub fn new() -> Self { Parser }\n}\n\nimpl Display for Parser {\n    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { Ok(()) }\n}\n";
        let s = scan("src/parser.rs", src);
        let test = s
            .items
            .iter()
            .find(|i| i.name == "check_roundtrip")
            .unwrap();
        assert_eq!(test.kind, ScanKind::Test);
        let impl1 = s
            .items
            .iter()
            .find(|i| i.kind == ScanKind::Impl && i.name == "Parser")
            .unwrap();
        assert_eq!(impl1.impl_target.as_deref(), Some("Parser"));
        let impl2 = s
            .items
            .iter()
            .find(|i| i.kind == ScanKind::Impl && i.name == "Display for Parser")
            .unwrap();
        assert_eq!(impl2.impl_trait.as_deref(), Some("Display"));
        assert_eq!(impl2.impl_target.as_deref(), Some("Parser"));
    }

    #[test]
    fn use_declarations() {
        let src = "use crate::parser::Name;\nuse std::collections::{HashMap, HashSet};\nuse std::io as io;\n";
        let s = scan("src/parser.rs", src);
        let paths: Vec<&str> = s.uses.iter().map(|u| u.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["crate::parser::Name", "std::collections", "std::io"]
        );
    }

    #[test]
    fn type_aliases_consts_and_statics() {
        let src = "type Result<T> = std::result::Result<T, Error>;\nconst MAX: usize = 16;\nstatic NAME: &str = \"x\";\n";
        let s = scan("src/parser.rs", src);
        let kinds: Vec<ScanKind> = s.items.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            vec![ScanKind::Type, ScanKind::Constant, ScanKind::Static]
        );
        let ty = &s.items[0];
        assert!(ty.signature.contains("type Result<T> ="));
    }

    #[test]
    fn generic_functions_and_macro_rules() {
        let src = "fn map<T, U>(f: impl Fn(T) -> U) -> Vec<U> { vec![] }\nmacro_rules! my_macro { () => {} }\n";
        let s = scan("src/parser.rs", src);
        let names: Vec<&str> = s.items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["map", "my_macro"]);
    }

    #[test]
    fn visibility_kinds() {
        let src = "pub(crate) fn semi() {}\npub fn full() {}\nfn priv_fn() {}\n";
        let s = scan("src/parser.rs", src);
        assert_eq!(s.items[0].visibility, "crate");
        assert_eq!(s.items[1].visibility, "public");
        assert_eq!(s.items[2].visibility, "private");
    }

    #[test]
    fn nested_block_comments() {
        let src = "/* outer /* inner */ still comment */\nfn after() {}\n";
        let s = scan("src/parser.rs", src);
        assert_eq!(s.items.len(), 1);
        assert_eq!(s.items[0].name, "after");
    }

    #[test]
    fn determinism() {
        let src = "pub fn a() {}\nmod m { pub fn b() {} }\n";
        let s1 = scan("src/parser.rs", src);
        let s2 = scan("src/parser.rs", src);
        assert_eq!(s1.items, s2.items);
        assert_eq!(s1.uses, s2.uses);
    }
}
