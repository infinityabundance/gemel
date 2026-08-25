//! Semantic indexing (Phase 5, SPECIFICATION.md Phase 5).
//!
//! Language-aware entities are *derived facts*: deterministic observations
//! of declarations in source, recorded as canonical `semantic-entity`
//! objects per state, grouped by a canonical `semantic-index` object. The
//! hierarchy is respected (SPECIFICATION.md §21):
//!
//! ```text
//! observed evidence (source bytes)
//!     > deterministically derived facts (entity objects)
//!     > declared claims (lineage certainty)
//! ```
//!
//! Identity is content-addressed over the full canonical description.
//! Unchanged entities deduplicate across states by content; changed or moved
//! entities become new objects linked by **explicit lineage** with a
//! documented evidence string and certainty — a permanent semantic identity
//! is never silently inferred from heuristics (brief §22).

pub mod rust;

use crate::family::Family;
use crate::gid::Gid;
use crate::store::refs::{RefOp, RefTransaction};
use crate::store::{Error, Repo, REF_HEAD};
use crate::value::{Field, Object, Value};
use std::collections::BTreeMap;

/// The ref namespace for per-state semantic indexes.
pub const REF_SEMANTIC: &str = "refs/semantic";
/// The ref pointing at the most recently built index.
pub const REF_SEMANTIC_CURRENT: &str = "refs/semantic/current";
/// The ref pointing at the most recently built index for the head state.
pub const REF_SEMANTIC_HEAD: &str = "refs/semantic/head";

/// The deterministic indexer producer identity name.
pub const INDEXER_PRODUCER_NAME: &str = "semantic-indexer";

/// The canonical entity kind strings (a superset of the scanner's kinds).
pub fn entity_kind_of(scan_kind: rust::ScanKind) -> &'static str {
    scan_kind.as_str()
}

/// One entity to be published.
#[derive(Debug, Clone)]
pub struct EntityRecord {
    pub kind: String,
    pub name: String,
    pub module_path: String,
    pub file_path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub signature: String,
    pub visibility: String,
    pub parent_name: Option<String>,
    pub impl_target: Option<String>,
    pub impl_trait: Option<String>,
    pub state: Gid,
    pub uses: Vec<String>,
}

/// The outcome of indexing one state.
#[derive(Debug, Clone)]
pub struct IndexOutcome {
    pub entities: usize,
    pub files: usize,
    pub new_entities: usize,
    pub modified_entities: usize,
    pub moved_entities: usize,
    pub lineage_links: usize,
    pub index: Gid,
}

/// Builds (or rebuilds) the semantic index of `state`.
///
/// Deterministic: identical state bytes produce identical entity objects and
/// the identical index object. Lineage is computed against the previous
/// indexed state when one is known.
pub fn index_state(repo: &Repo, state: &Gid, producer: &Gid) -> Result<IndexOutcome, Error> {
    let (records, file_count) = records_for_state(repo, state)?;
    // Previous index for lineage.
    let previous: Vec<(Gid, Object)> = previous_index_entities(repo, state)?;
    let prev_by_key: BTreeMap<(String, String, String), Gid> = previous
        .iter()
        .filter_map(|(gid, obj)| {
            let fs = obj.field_sequence()?;
            Some((
                (
                    crate::query::str_field(fs, 0x01)?.to_string(),
                    crate::query::str_field(fs, 0x02)?.to_string(),
                    crate::query::str_field(fs, 0x03)?.to_string(),
                ),
                *gid,
            ))
        })
        .collect();
    let prev_by_name: BTreeMap<(String, String), Vec<Gid>> = {
        let mut m: BTreeMap<(String, String), Vec<Gid>> = BTreeMap::new();
        for (gid, obj) in &previous {
            if let Some(fs) = obj.field_sequence() {
                if let (Some(k), Some(n)) = (
                    crate::query::str_field(fs, 0x01),
                    crate::query::str_field(fs, 0x02),
                ) {
                    m.entry((k.to_string(), n.to_string()))
                        .or_default()
                        .push(*gid);
                }
            }
        }
        m
    };

    let mut published: Vec<Gid> = Vec::new();
    let mut new_entities = 0usize;
    let mut modified_entities = 0usize;
    let mut moved_entities = 0usize;
    let mut lineage_links = 0usize;
    // The indexer producer must be published (deduplicated by content) so
    // entity/index references never dangle: fsck reachability and exchange
    // export walk gid edges and treat missing referents as corruption.
    let indexer_gid = repo.insert_object(&crate::defaults::automation_producer_object_at(
        INDEXER_PRODUCER_NAME,
        0,
    ))?;
    for r in &records {
        let obj = build_entity_object(r, None, indexer_gid)?;
        let gid = crate::content::object_identity(repo, &obj)?;
        // Unchanged entities deduplicate by content (same id exists).
        let exists = repo.read_object(&gid).is_ok();
        if exists {
            published.push(gid);
            continue;
        }
        // Lineage: match against the previous index.
        let key = (r.kind.clone(), r.name.clone(), r.module_path.clone());
        let lineage: Option<(Gid, String, &str)> = if let Some(prev) = prev_by_key.get(&key) {
            Some((*prev, "same-name-kind-path".to_string(), "observed"))
        } else if let Some(candidates) = prev_by_name.get(&(r.kind.clone(), r.name.clone())) {
            // Same kind+name in a different module: a move — possible lineage.
            let best = candidates
                .iter()
                .filter_map(|g| {
                    previous
                        .iter()
                        .find(|(gid, _)| gid == g)
                        .and_then(|(_, o)| {
                            let fs = o.field_sequence()?;
                            let sig = crate::query::str_field(fs, 0x07).unwrap_or("");
                            Some((*g, sig.to_string()))
                        })
                })
                .max_by_key(|(_, sig)| common_prefix_len(sig, &r.signature));
            match best {
                Some((g, _)) => Some((g, "similarity:same-name-kind".to_string(), "possible")),
                None => None,
            }
        } else {
            None
        };
        let obj = match lineage {
            Some((from, evidence, certainty)) => {
                lineage_links += 1;
                if prev_by_key.contains_key(&key) {
                    modified_entities += 1;
                } else {
                    moved_entities += 1;
                }
                build_entity_object(r, Some((from, evidence, certainty)), indexer_gid)?
            }
            None => {
                new_entities += 1;
                obj
            }
        };
        let gid = repo.insert_object(&obj)?;
        published.push(gid);
    }
    let index_obj = Object::fields(
        Family::SemanticIndex,
        vec![
            Field::new(0x01, Value::Gid(*state)),
            Field::new(
                0x02,
                Value::Array(published.iter().copied().map(Value::Gid).collect()),
            ),
            Field::new(0x03, Value::Gid(*producer)),
            Field::new(0x04, Value::I(0)),
        ],
    );
    let index = repo.insert_object(&index_obj)?;
    let state_hex = crate::hex::encode(state.digest());
    let ops = vec![
        RefOp::set(&format!("{REF_SEMANTIC}/state/{state_hex}"), index),
        RefOp::set(REF_SEMANTIC_CURRENT, index),
    ];
    let head = repo.read_ref(REF_HEAD)?;
    let is_head = head.is_some() && {
        let h = head.unwrap();
        let obj = repo.load(&h)?;
        let fs = obj.field_sequence().unwrap_or(&[]);
        crate::query::gid_field(fs, 0x05) == Some(*state)
    };
    let ops = if is_head {
        vec![
            RefOp::set(&format!("{REF_SEMANTIC}/state/{state_hex}"), index),
            RefOp::set(REF_SEMANTIC_CURRENT, index),
            RefOp::set(REF_SEMANTIC_HEAD, index),
        ]
    } else {
        ops
    };
    repo.with_write_lock(|| {
        repo.apply_refs_unlocked(&RefTransaction { ops })?;
        Ok(())
    })?;
    Ok(IndexOutcome {
        entities: records.len(),
        files: file_count,
        new_entities,
        modified_entities,
        moved_entities,
        lineage_links,
        index,
    })
}

/// Scans the state's source files into deterministic entity records. Used by
/// both the indexer (which publishes) and the semantic diff (which must not
/// publish anything).
pub fn records_for_state(repo: &Repo, state: &Gid) -> Result<(Vec<EntityRecord>, usize), Error> {
    let files = crate::content::state_files(repo, state)?;
    let mut records: Vec<EntityRecord> = Vec::new();
    let mut file_count = 0usize;
    for (path, (mode, blob)) in &files {
        if mode & 0o170000 == 0o120000 {
            continue; // symlinks are not source content
        }
        if path.ends_with(".rs") {
            let content = repo
                .load(blob)?
                .blob_bytes()
                .ok_or_else(|| Error::Invalid(format!("{blob} is not a blob")))?
                .to_vec();
            let scan = rust::scan_file(path, &content)?;
            file_count += 1;
            for item in &scan.items {
                records.push(EntityRecord {
                    kind: entity_kind_of(item.kind).to_string(),
                    name: item.name.clone(),
                    module_path: item.module_path.clone(),
                    file_path: path.clone(),
                    start_line: item.start_line,
                    end_line: item.end_line,
                    signature: item.signature.clone(),
                    visibility: item.visibility.clone(),
                    parent_name: item.impl_target.clone().or_else(|| item.impl_trait.clone()),
                    impl_target: item.impl_target.clone(),
                    impl_trait: item.impl_trait.clone(),
                    state: *state,
                    uses: Vec::new(),
                });
            }
            // Module-level `use` declarations become dependency records of
            // the file's module entity when one exists.
            if !scan.uses.is_empty() {
                let mod_entity = records
                    .iter_mut()
                    .find(|r| r.kind == "module" && r.module_path == scan.file_module_path);
                match mod_entity {
                    Some(m) => m.uses.extend(scan.uses.iter().map(|u| u.path.clone())),
                    None => {
                        // No module entity declared for this file: record the
                        // uses on the synthetic module record.
                        records.push(EntityRecord {
                            kind: "module".to_string(),
                            name: scan
                                .file_module_path
                                .rsplit("::")
                                .next()
                                .unwrap_or("crate")
                                .to_string(),
                            module_path: scan.file_module_path.clone(),
                            file_path: path.clone(),
                            start_line: 1,
                            end_line: 1,
                            signature: String::new(),
                            visibility: "private".to_string(),
                            parent_name: None,
                            impl_target: None,
                            impl_trait: None,
                            state: *state,
                            uses: scan.uses.iter().map(|u| u.path.clone()).collect(),
                        });
                    }
                }
            }
        } else if path == "Cargo.toml" {
            let content = repo
                .load(blob)?
                .blob_bytes()
                .ok_or_else(|| Error::Invalid(format!("{blob} is not a blob")))?
                .to_vec();
            let toml = toml_scan(&String::from_utf8_lossy(&content));
            for (feature, deps) in toml.features {
                records.push(EntityRecord {
                    kind: "feature".to_string(),
                    name: feature,
                    module_path: "crate".to_string(),
                    file_path: path.clone(),
                    start_line: 1,
                    end_line: 1,
                    signature: String::new(),
                    visibility: "public".to_string(),
                    parent_name: None,
                    impl_target: None,
                    impl_trait: None,
                    state: *state,
                    uses: deps,
                });
            }
            for dep in toml.dependencies {
                records.push(EntityRecord {
                    kind: "dependency".to_string(),
                    name: dep,
                    module_path: "crate".to_string(),
                    file_path: path.clone(),
                    start_line: 1,
                    end_line: 1,
                    signature: String::new(),
                    visibility: "public".to_string(),
                    parent_name: None,
                    impl_target: None,
                    impl_trait: None,
                    state: *state,
                    uses: Vec::new(),
                });
            }
        }
    }
    // Deterministic order.
    records.sort_by(|a, b| {
        a.module_path
            .cmp(&b.module_path)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    Ok((records, file_count))
}

/// Builds the canonical entity object. `lineage` is Some for changed/moved
/// entities: (previous entity gid, evidence string, certainty). `indexer_gid`
/// is the published producer identity of the deterministic indexer.
fn build_entity_object(
    r: &EntityRecord,
    lineage: Option<(Gid, String, &str)>,
    indexer_gid: Gid,
) -> Result<Object, Error> {
    let mut fields = vec![
        Field::new(0x01, Value::Str(r.kind.clone())),
        Field::new(0x02, Value::Str(r.name.clone())),
        Field::new(0x03, Value::Str(r.module_path.clone())),
    ];
    if !r.file_path.is_empty() {
        fields.push(Field::new(0x04, Value::Str(r.file_path.clone())));
    }
    if r.start_line > 0 {
        fields.push(Field::new(0x05, Value::U(r.start_line)));
    }
    if r.end_line > 0 {
        fields.push(Field::new(0x06, Value::U(r.end_line)));
    }
    if !r.signature.is_empty() {
        fields.push(Field::new(0x07, Value::Str(r.signature.clone())));
    }
    fields.push(Field::new(0x08, Value::Str(r.visibility.clone())));
    if let Some((from, evidence, certainty)) = lineage {
        fields.push(Field::new(0x0A, Value::Gid(from)));
        fields.push(Field::new(0x0B, Value::Str(evidence)));
        fields.push(Field::new(0x0C, Value::Str(certainty.to_string())));
    }
    fields.push(Field::new(0x0E, Value::Gid(r.state)));
    fields.push(Field::new(0x0F, Value::Gid(indexer_gid)));
    fields.push(Field::new(0x10, Value::I(0)));
    Ok(Object::fields(Family::SemanticEntity, fields))
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// The entity objects of the previously indexed state (the direct parent of
/// `state` in the head chain, or the current index when known).
fn previous_index_entities(repo: &Repo, state: &Gid) -> Result<Vec<(Gid, Object)>, Error> {
    let mut out = Vec::new();
    let current = repo.read_ref(REF_SEMANTIC_CURRENT)?;
    let index = match current {
        Some(i) => i,
        None => return Ok(out),
    };
    let obj = repo.load(&index)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    let idx_state = crate::query::gid_field(fs, 0x01);
    if idx_state == Some(*state) {
        return Ok(out); // same state: nothing to compare against
    }
    for gid in crate::query::gid_list(fs, 0x02) {
        if let Ok(o) = repo.load(&gid) {
            out.push((gid, o));
        }
    }
    Ok(out)
}

/// The index gid for a state, if indexed.
pub fn index_for_state(repo: &Repo, state: &Gid) -> Result<Option<Gid>, Error> {
    let hex = crate::hex::encode(state.digest());
    repo.read_ref(&format!("{REF_SEMANTIC}/state/{hex}"))
}

/// The entity objects of a state's index, if indexed.
pub fn entities_for_state(repo: &Repo, state: &Gid) -> Result<Option<Vec<(Gid, Object)>>, Error> {
    let Some(index) = index_for_state(repo, state)? else {
        return Ok(None);
    };
    let obj = repo.load(&index)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    let mut out = Vec::new();
    for gid in crate::query::gid_list(fs, 0x02) {
        out.push((gid, repo.load(&gid)?));
    }
    Ok(Some(out))
}

/// The current index's entities (latest indexed state).
pub fn current_entities(repo: &Repo) -> Result<Option<Vec<(Gid, Object)>>, Error> {
    let Some(index) = repo.read_ref(REF_SEMANTIC_CURRENT)? else {
        return Ok(None);
    };
    let obj = repo.load(&index)?;
    let fs = obj.field_sequence().unwrap_or(&[]);
    let state = crate::query::gid_field(fs, 0x01);
    let Some(state) = state else {
        return Ok(None);
    };
    entities_for_state(repo, &state)
}

/// Resolves a subject to a semantic entity: full path, `path::name` suffix,
/// bare name, or a `semantic-entity` gid.
pub fn resolve_entity(repo: &Repo, subject: &str) -> Result<Option<(Gid, Object)>, Error> {
    if let Ok(gid) = subject.parse::<Gid>() {
        if gid.family() == Family::SemanticEntity {
            if let Ok(obj) = repo.load(&gid) {
                return Ok(Some((gid, obj)));
            }
        }
    }
    let Some(entities) = current_entities(repo)? else {
        return Ok(None);
    };
    let mut matches: Vec<(Gid, Object, String)> = Vec::new();
    for (gid, obj) in &entities {
        let fs = obj.field_sequence().unwrap_or(&[]);
        let name = crate::query::str_field(fs, 0x02).unwrap_or("");
        let mp = crate::query::str_field(fs, 0x03).unwrap_or("");
        let full = if mp == "crate" {
            format!("crate::{name}")
        } else {
            format!("{mp}::{name}")
        };
        if full == subject || full.ends_with(&format!("::{subject}")) || name == subject {
            matches.push((*gid, obj.clone(), full));
        }
    }
    matches.sort_by(|a, b| a.2.cmp(&b.2));
    Ok(matches.into_iter().next().map(|(g, o, _)| (g, o)))
}

/// The resolved view of one semantic entity (used by queries and the CLI).
#[derive(Debug, Clone)]
pub struct EntityInfo {
    pub id: Option<Gid>,
    pub kind: String,
    pub name: String,
    pub module_path: String,
    pub file_path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub signature: String,
    pub visibility: String,
    /// (lineage_from gid, evidence, certainty).
    pub lineage: Option<(Gid, String, String)>,
    pub state: Gid,
}

impl EntityInfo {
    /// The canonical full path (`crate::parser::decode_name`).
    pub fn full_path(&self) -> String {
        if self.module_path == "crate" {
            format!("crate::{}", self.name)
        } else {
            format!("{}::{}", self.module_path, self.name)
        }
    }

    pub fn from_object(obj: &Object) -> EntityInfo {
        let fs = obj.field_sequence().unwrap_or(&[]);
        let lineage = crate::query::gid_field(fs, 0x0A).map(|from| {
            (
                from,
                crate::query::str_field(fs, 0x0B).unwrap_or("").to_string(),
                crate::query::str_field(fs, 0x0C)
                    .unwrap_or("unknown")
                    .to_string(),
            )
        });
        EntityInfo {
            id: None, // filled by the caller when known
            kind: crate::query::str_field(fs, 0x01).unwrap_or("").to_string(),
            name: crate::query::str_field(fs, 0x02).unwrap_or("").to_string(),
            module_path: crate::query::str_field(fs, 0x03).unwrap_or("").to_string(),
            file_path: crate::query::str_field(fs, 0x04).unwrap_or("").to_string(),
            start_line: crate::query::u64_field(fs, 0x05).unwrap_or(0),
            end_line: crate::query::u64_field(fs, 0x06).unwrap_or(0),
            signature: crate::query::str_field(fs, 0x07).unwrap_or("").to_string(),
            visibility: crate::query::str_field(fs, 0x08)
                .unwrap_or("unknown")
                .to_string(),
            lineage,
            state: crate::query::gid_field(fs, 0x0E)
                .unwrap_or(Gid::new(crate::family::Family::State, [0u8; 32])),
        }
    }
}

/// The published entity info of an indexed entity object.
pub fn entity_info(repo: &Repo, gid: &Gid) -> Result<EntityInfo, Error> {
    let obj = repo.load(gid)?;
    let mut info = EntityInfo::from_object(&obj);
    info.id = Some(*gid);
    Ok(info)
}

/// Resolves an entity at a specific line of a file in the current index
/// (`gemel why src/name.rs:417`): the entity whose span contains the line.
pub fn resolve_entity_at(
    repo: &Repo,
    path: &str,
    line: u64,
) -> Result<Option<(Gid, Object)>, Error> {
    let Some(entities) = current_entities(repo)? else {
        return Ok(None);
    };
    let mut best: Option<(Gid, Object, u64)> = None; // smallest containing span wins
    for (gid, obj) in &entities {
        let fs = obj.field_sequence().unwrap_or(&[]);
        if crate::query::str_field(fs, 0x04) != Some(path) {
            continue;
        }
        let start = crate::query::u64_field(fs, 0x05).unwrap_or(0);
        let end = crate::query::u64_field(fs, 0x06).unwrap_or(0);
        if start > 0 && start <= line && (end == 0 || line <= end) {
            let span = end.saturating_sub(start);
            if best.as_ref().map(|(_, _, s)| span < *s).unwrap_or(true) {
                best = Some((*gid, obj.clone(), span));
            }
        }
    }
    Ok(best.map(|(g, o, _)| (g, o)))
}

/// The lineage chain of an entity: itself first, then `lineage_from` up to a
/// bounded depth (brief §22: explicit lineage, never a silent merge).
pub fn lineage_chain(repo: &Repo, entity_gid: &Gid) -> Result<Vec<(Gid, Object)>, Error> {
    let mut chain = Vec::new();
    let mut current = Some(*entity_gid);
    let mut depth = 0u32;
    while let Some(g) = current {
        if depth > 64 {
            break;
        }
        let obj = match repo.load(&g) {
            Ok(o) => o,
            Err(_) => break,
        };
        chain.push((g, obj.clone()));
        current = obj
            .field_sequence()
            .and_then(|fs| crate::query::gid_field(fs, 0x0A));
        depth += 1;
    }
    Ok(chain)
}

/// The result of resolving a query subject: the entity (when one resolves)
/// and the alias strings that should also match changes/claims.
#[derive(Debug, Clone, Default)]
pub struct ResolvedSubject {
    pub entity: Option<EntityInfo>,
    pub aliases: Vec<String>,
}

/// Resolves a query subject (bare name, `path::name`, `file:line`, or a
/// semantic-entity gid) into an entity and its aliases. Aliases include the
/// entity's gid, name, module path, full path, file path, and — walking the
/// explicit lineage chain — the previous names/module paths/file paths, so
/// queries about an entity also surface the work that touched its ancestors
/// (brief §22: file-movement survival without heuristic identity).
pub fn resolve_subject(repo: &Repo, subject: &str) -> Result<ResolvedSubject, Error> {
    let mut out = ResolvedSubject::default();
    let mut push = |a: String| {
        if !a.is_empty() && !out.aliases.contains(&a) {
            out.aliases.push(a);
        }
    };
    let mut entity: Option<(Gid, Object)> = None;
    // `file:line` form.
    if let Some((path, line)) = split_file_line(subject) {
        push(path.clone());
        if let Ok(line) = line.parse::<u64>() {
            entity = resolve_entity_at(repo, &path, line)?;
        }
    }
    if entity.is_none() {
        entity = resolve_entity(repo, subject)?;
    }
    if let Some((gid, obj)) = &entity {
        let mut info = EntityInfo::from_object(obj);
        info.id = Some(*gid);
        push(gid.to_string());
        push(info.name.clone());
        push(info.module_path.clone());
        push(info.full_path());
        push(info.file_path.clone());
        for (from_gid, from_obj) in lineage_chain(repo, gid)? {
            if from_gid == *gid {
                continue;
            }
            let f = EntityInfo::from_object(&from_obj);
            push(f.name.clone());
            push(f.module_path.clone());
            push(f.full_path());
            push(f.file_path.clone());
        }
        out.entity = Some(info);
    }
    Ok(out)
}

/// Splits `path:line` into (path, line) when the part after the last `:` is
/// a decimal line number; otherwise None (gids contain no `:`; neither do
/// module paths or bare names).
fn split_file_line(subject: &str) -> Option<(String, String)> {
    let (path, line) = subject.rsplit_once(':')?;
    if line.is_empty() || !line.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((path.to_string(), line.to_string()))
}

/// One entity delta between two states.
#[derive(Debug, Clone)]
pub struct EntityDelta {
    pub before: Option<EntityInfo>,
    pub after: Option<EntityInfo>,
}

/// The semantic diff between two states (brief §23: `gemel diff --semantic`).
/// Moves are reported only through explicit recorded lineage (lineage_from
/// pointing into the earlier state); everything else is added/removed/modified
/// by exact content identity. No heuristic identity is invented.
#[derive(Debug, Clone)]
pub struct SemanticDiff {
    pub state_a: Gid,
    pub state_b: Gid,
    pub added: Vec<EntityInfo>,
    pub removed: Vec<EntityInfo>,
    pub modified: Vec<EntityDelta>,
    pub moved: Vec<EntityDelta>,
    pub unchanged: usize,
}

/// Computes the semantic diff between two states. Both sides are scanned
/// deterministically; when a state has a published index its lineage fields
/// are honored, otherwise the raw records are used (lineage unknown).
pub fn semantic_diff(repo: &Repo, a: &Gid, b: &Gid) -> Result<SemanticDiff, Error> {
    let infos_a = entity_infos_for(repo, a)?;
    let infos_b = entity_infos_for(repo, b)?;
    let mut diff = SemanticDiff {
        state_a: *a,
        state_b: *b,
        added: Vec::new(),
        removed: Vec::new(),
        modified: Vec::new(),
        moved: Vec::new(),
        unchanged: 0,
    };
    // Content identity: kind/name/module/file/signature/visibility plus a
    // digest of the entity's source lines (so body-only edits register), with
    // the state reference neutralized — an entity's semantic shape must not
    // differ merely because it belongs to a different state.
    let files_a = file_contents_of(repo, a)?;
    let files_b = file_contents_of(repo, b)?;
    let ident = |info: &EntityInfo,
                 files: &std::collections::BTreeMap<String, Vec<u8>>|
     -> Result<Vec<u8>, Error> {
        let body = match files.get(&info.file_path) {
            Some(content) => line_digest(content, info.start_line, info.end_line),
            None => Vec::new(),
        };
        let rec = EntityRecord {
            kind: info.kind.clone(),
            name: info.name.clone(),
            module_path: info.module_path.clone(),
            file_path: info.file_path.clone(),
            start_line: info.start_line,
            end_line: info.end_line,
            signature: info.signature.clone(),
            visibility: info.visibility.clone(),
            parent_name: None,
            impl_target: None,
            impl_trait: None,
            state: Gid::new(Family::State, [0u8; 32]),
            uses: Vec::new(),
        };
        let obj = build_entity_object(&rec, None, Gid::new(Family::Producer, [0u8; 32]))?;
        let mut bytes = crate::encode::encode_object(&obj, &repo.limits())?;
        bytes.extend_from_slice(&body);
        Ok(bytes)
    };
    let mut sig_a: Vec<(Vec<u8>, EntityInfo)> = infos_a
        .iter()
        .map(|i| ident(i, &files_a).map(|b| (b, i.clone())))
        .collect::<Result<_, _>>()?;
    let mut sig_b: Vec<(Vec<u8>, EntityInfo)> = infos_b
        .iter()
        .map(|i| ident(i, &files_b).map(|b| (b, i.clone())))
        .collect::<Result<_, _>>()?;
    // unchanged: identical content on both sides (multiset subtraction).
    sig_a.sort_by(|x, y| x.0.cmp(&y.0));
    sig_b.sort_by(|x, y| x.0.cmp(&y.0));
    let mut i = 0usize;
    let mut j = 0usize;
    let mut unchanged = 0usize;
    let mut a_rest: Vec<EntityInfo> = Vec::new();
    let mut b_rest: Vec<EntityInfo> = Vec::new();
    while i < sig_a.len() && j < sig_b.len() {
        match sig_a[i].0.cmp(&sig_b[j].0) {
            std::cmp::Ordering::Less => {
                a_rest.push(sig_a[i].1.clone());
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                b_rest.push(sig_b[j].1.clone());
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                unchanged += 1;
                i += 1;
                j += 1;
            }
        }
    }
    while i < sig_a.len() {
        a_rest.push(sig_a[i].1.clone());
        i += 1;
    }
    while j < sig_b.len() {
        b_rest.push(sig_b[j].1.clone());
        j += 1;
    }
    diff.unchanged = unchanged;
    // moved: B entities whose recorded lineage_from targets an A entity AND
    // whose (kind, name, module_path) key is absent from A (a genuine move;
    // same-key entities are modifications below).
    let a_keys: std::collections::HashSet<(String, String, String)> = infos_a
        .iter()
        .map(|x| (x.kind.clone(), x.name.clone(), x.module_path.clone()))
        .collect();
    let a_by_gid: std::collections::HashMap<Gid, EntityInfo> = infos_a
        .iter()
        .filter_map(|x| x.id.map(|g| (g, x.clone())))
        .collect();
    let mut moved_b: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut moved_from: std::collections::HashSet<Gid> = std::collections::HashSet::new();
    for (bi, info) in b_rest.iter().enumerate() {
        let key = (
            info.kind.clone(),
            info.name.clone(),
            info.module_path.clone(),
        );
        if a_keys.contains(&key) {
            continue;
        }
        if let Some((from, _, _)) = &info.lineage {
            if let Some(before) = a_by_gid.get(from) {
                diff.moved.push(EntityDelta {
                    before: Some(before.clone()),
                    after: Some(info.clone()),
                });
                moved_b.insert(bi);
                if let Some(id) = before.id {
                    moved_from.insert(id);
                }
            }
        }
    }
    // modified: same (kind, name, module_path) key on both sides with
    // different content. Pair by key group in deterministic order; the B
    // side's lineage (when present) records the observed relationship.
    let key_of = |info: &EntityInfo| {
        (
            info.kind.clone(),
            info.name.clone(),
            info.module_path.clone(),
        )
    };
    let mut idx_a: std::collections::HashMap<(String, String, String), Vec<usize>> =
        std::collections::HashMap::new();
    for (ai, info) in a_rest.iter().enumerate() {
        idx_a.entry(key_of(info)).or_default().push(ai);
    }
    let mut idx_b: std::collections::HashMap<(String, String, String), Vec<usize>> =
        std::collections::HashMap::new();
    for (bi, info) in b_rest.iter().enumerate() {
        if moved_b.contains(&bi) {
            continue;
        }
        idx_b.entry(key_of(info)).or_default().push(bi);
    }
    let mut b_matched: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut a_matched: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut keys: Vec<(String, String, String)> =
        idx_a.keys().cloned().chain(idx_b.keys().cloned()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let (Some(la), Some(lb)) = (idx_a.get(&key), idx_b.get(&key)) else {
            continue;
        };
        let pairs = la.len().min(lb.len());
        for k in 0..pairs {
            let (ai, bi) = (la[k], lb[k]);
            diff.modified.push(EntityDelta {
                before: Some(a_rest[ai].clone()),
                after: Some(b_rest[bi].clone()),
            });
            a_matched.insert(ai);
            b_matched.insert(bi);
        }
    }
    // remaining B: added; remaining A: removed.
    diff.added = b_rest
        .iter()
        .enumerate()
        .filter(|(bi, _)| !moved_b.contains(bi) && !b_matched.contains(bi))
        .map(|(_, x)| x.clone())
        .collect();
    diff.removed = a_rest
        .iter()
        .enumerate()
        .filter(|(ai, _)| {
            !a_matched.contains(ai)
                && !a_rest[*ai]
                    .id
                    .map(|g| moved_from.contains(&g))
                    .unwrap_or(false)
        })
        .map(|(_, x)| x.clone())
        .collect();
    diff.added.sort_by_key(|x| x.full_path());
    diff.removed.sort_by_key(|x| x.full_path());
    diff.modified.sort_by(|x, y| {
        x.after
            .as_ref()
            .map(|i| i.full_path())
            .unwrap_or_default()
            .cmp(&y.after.as_ref().map(|i| i.full_path()).unwrap_or_default())
    });
    diff.moved.sort_by(|x, y| {
        x.after
            .as_ref()
            .map(|i| i.full_path())
            .unwrap_or_default()
            .cmp(&y.after.as_ref().map(|i| i.full_path()).unwrap_or_default())
    });
    Ok(diff)
}

/// The entity infos of a state: the published index when available, otherwise
/// a raw deterministic scan (never publishing).
fn entity_infos_for(repo: &Repo, state: &Gid) -> Result<Vec<EntityInfo>, Error> {
    if let Some(entities) = entities_for_state(repo, state)? {
        let mut out = Vec::new();
        for (gid, obj) in entities {
            let mut info = EntityInfo::from_object(&obj);
            info.id = Some(gid);
            out.push(info);
        }
        return Ok(out);
    }
    let (records, _) = records_for_state(repo, state)?;
    Ok(records
        .iter()
        .map(|r| EntityInfo {
            id: None,
            kind: r.kind.clone(),
            name: r.name.clone(),
            module_path: r.module_path.clone(),
            file_path: r.file_path.clone(),
            start_line: r.start_line,
            end_line: r.end_line,
            signature: r.signature.clone(),
            visibility: r.visibility.clone(),
            lineage: None,
            state: r.state,
        })
        .collect())
}

/// The source file contents of a state, keyed by path (symlinks excluded).
fn file_contents_of(
    repo: &Repo,
    state: &Gid,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, Error> {
    let mut out = std::collections::BTreeMap::new();
    for (path, (mode, blob)) in crate::content::state_files(repo, state)? {
        if mode & 0o170000 == 0o120000 {
            continue; // symlinks are not source content
        }
        if let Ok(content) = repo.load(&blob).and_then(|o| {
            o.blob_bytes()
                .map(|b| b.to_vec())
                .ok_or_else(|| Error::Invalid(format!("{blob} is not a blob")))
        }) {
            out.insert(path.clone(), content);
        }
    }
    Ok(out)
}

/// The BLAKE3 digest of the source lines `[start..=end]` (1-based), or empty
/// when the span is unknown.
fn line_digest(content: &[u8], start: u64, end: u64) -> Vec<u8> {
    if start == 0 || end == 0 || end < start {
        return Vec::new();
    }
    let mut line = 1u64;
    let mut begin = None;
    let mut stop = content.len();
    for (i, b) in content.iter().enumerate() {
        if line == start && begin.is_none() {
            begin = Some(i);
        }
        if *b == b'\n' {
            if line == end {
                stop = i; // exclude the trailing newline of the end line
                break;
            }
            line += 1;
        }
    }
    let Some(begin) = begin else {
        return Vec::new();
    };
    if begin >= stop {
        return Vec::new();
    }
    crate::hash::blake3_256(&content[begin..stop]).to_vec()
}

/// A minimal `Cargo.toml` section scanner: `[features]` and top-level
/// `[dependencies]`/`[dev-dependencies]` names. Deterministic; comments and
/// quoted strings are skipped.
pub struct TomlScan {
    pub features: Vec<(String, Vec<String>)>,
    pub dependencies: Vec<String>,
}

pub fn toml_scan(text: &str) -> TomlScan {
    let mut features = Vec::new();
    let mut dependencies = Vec::new();
    let mut section = String::new();
    for raw_line in text.lines() {
        let line = strip_toml_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                section = name.trim().to_string();
                continue;
            }
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        let value = value.trim();
        match section.as_str() {
            "features" => {
                // feature = ["dep1", "dep2"] or feature = []
                let mut deps = Vec::new();
                let inner = value.trim_start_matches('[').trim_end_matches(']');
                for item in inner.split(',') {
                    let item = item.trim().trim_matches('"').trim();
                    if !item.is_empty() {
                        deps.push(item.to_string());
                    }
                }
                features.push((key, deps));
            }
            "dependencies" | "dev-dependencies" | "build-dependencies" => {
                // name = { version = ".." } or name = ".." or name = { path = ".." }
                let name = key;
                dependencies.push(name);
            }
            _ => {}
        }
    }
    TomlScan {
        features,
        dependencies,
    }
}

fn strip_toml_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_derivation() {
        assert_eq!(rust::module_path_for_file("src/lib.rs"), "crate");
        assert_eq!(rust::module_path_for_file("src/main.rs"), "crate");
        assert_eq!(rust::module_path_for_file("src/parser.rs"), "crate::parser");
        assert_eq!(
            rust::module_path_for_file("src/dns/name/mod.rs"),
            "crate::dns::name"
        );
        assert_eq!(
            rust::module_path_for_file("src/dns/name/parser.rs"),
            "crate::dns::name::parser"
        );
        assert_eq!(rust::module_path_for_file("lib.rs"), "crate");
    }

    #[test]
    fn toml_features_and_dependencies() {
        let text = "# comment\n[features]\ndefault = [\"std\"]\nfull = [\"std\", \"serde\"]\n\n[dependencies]\nserde = { version = \"1\" }\nrand = \"0.8\"\n";
        let scan = toml_scan(text);
        assert_eq!(scan.features.len(), 2);
        assert_eq!(scan.features[0].0, "default");
        assert_eq!(scan.features[0].1, vec!["std"]);
        assert_eq!(scan.dependencies, vec!["serde", "rand"]);
    }
}
