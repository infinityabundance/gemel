//! Executable golden fixtures (OBJECT_MODEL.md §11–§12).
//!
//! Fixtures define representative canonical objects for every family,
//! including the acceptance-demo shape, chained objects, multi-parent
//! changes, and extension-field retention. The `golden-gen` binary pins them;
//! the test suite verifies bytes, identities, and cross-references.

use crate::encode::encode_object;
use crate::error::ObjectError;
use crate::hash::object_id_bytes;
use crate::value::{Field, Object, Value};
use crate::family::Family;
use crate::gid::Gid;
use crate::limits::Limits;
use std::collections::HashMap;

/// A fixture definition.
pub struct Fixture {
    pub name: &'static str,
    pub description: &'static str,
    pub build: fn(&mut FixtureCtx) -> Object,
}

/// Context for building fixtures: tracks built identities so fixtures can
/// reference each other by real pinned identities.
pub struct FixtureCtx {
    gids: HashMap<&'static str, Gid>,
    refs: Vec<(&'static str, Gid)>,
}

impl FixtureCtx {
    fn new() -> FixtureCtx {
        FixtureCtx {
            gids: HashMap::new(),
            refs: Vec::new(),
        }
    }

    /// The pinned identity of a previously built fixture.
    pub fn gid(&mut self, name: &'static str) -> Gid {
        let gid = *self
            .gids
            .get(name)
            .expect("referenced fixture not built yet");
        self.refs.push((name, gid));
        gid
    }

    fn insert(&mut self, name: &'static str, gid: Gid) {
        self.gids.insert(name, gid);
    }

    /// The references recorded during the current build.
    pub fn take_refs(&mut self) -> Vec<(&'static str, Gid)> {
        std::mem::take(&mut self.refs)
    }
}

/// A built fixture with its pinned identity and recorded references.
pub struct BuiltFixture {
    pub fixture: &'static Fixture,
    pub object: Object,
    pub gid: Gid,
    pub refs: Vec<(&'static str, Gid)>,
}

/// Builds all fixtures in dependency order.
pub fn build_all(limits: &Limits) -> Result<Vec<BuiltFixture>, ObjectError> {
    let mut ctx = FixtureCtx::new();
    let mut out = Vec::new();
    for fixture in all_fixtures() {
        let object = (fixture.build)(&mut ctx);
        let bytes = encode_object(&object, limits)?;
        let gid = Gid::new(object.family, object_id_bytes(&bytes));
        ctx.insert(fixture.name, gid);
        let refs = ctx.take_refs();
        out.push(BuiltFixture {
            fixture,
            object,
            gid,
            refs,
        });
    }
    Ok(out)
}

/// All fixtures, in dependency order (references must precede referents).
pub fn all_fixtures() -> &'static [Fixture] {
    FIXTURES
}

// ---------------------------------------------------------------------------
// Value construction helpers
// ---------------------------------------------------------------------------

const TS: i64 = 1_700_000_000_000;

fn f(tag: u8, value: Value) -> Field {
    Field::new(tag, value)
}
fn rec(fields: Vec<Field>) -> Value {
    Value::Record(fields)
}
fn arr(vals: Vec<Value>) -> Value {
    Value::Array(vals)
}
fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}
fn u(v: u64) -> Value {
    Value::U(v)
}
fn i(v: i64) -> Value {
    Value::I(v)
}
fn b(v: bool) -> Value {
    Value::B(v)
}
fn bytes(v: &[u8]) -> Value {
    Value::Bytes(v.to_vec())
}
fn g(gid: Gid) -> Value {
    Value::Gid(gid)
}
fn obj(family: Family, fields: Vec<Field>) -> Object {
    Object::fields(family, fields)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn build_blob_empty(_: &mut FixtureCtx) -> Object {
    Object::blob(Vec::new())
}

fn build_blob_hello(_: &mut FixtureCtx) -> Object {
    Object::blob(b"hi".to_vec())
}

fn build_blob_binary(_: &mut FixtureCtx) -> Object {
    Object::blob((0u8..=255).collect())
}

fn build_blob_link(_: &mut FixtureCtx) -> Object {
    Object::blob(b"../bin/tool".to_vec())
}

fn build_blob_lib(_: &mut FixtureCtx) -> Object {
    Object::blob(b"pub fn decode() {}\n".to_vec())
}

fn build_blob_script(_: &mut FixtureCtx) -> Object {
    Object::blob(b"#!/bin/sh\necho hi\n".to_vec())
}

fn build_producer_human(_: &mut FixtureCtx) -> Object {
    obj(
        Family::Producer,
        vec![
            f(0x01, s("human")),
            f(0x02, s("Ada Lovelace")),
            f(
                0x03,
                rec(vec![f(
                    0x01,
                    rec(vec![
                        f(0x01, s("Ada Lovelace")),
                        f(0x02, s("ada@example.invalid")),
                    ]),
                )]),
            ),
            f(0x04, s("FULL")),
            f(0x06, i(TS)),
        ],
    )
}

fn build_producer_agent(_: &mut FixtureCtx) -> Object {
    obj(
        Family::Producer,
        vec![
            f(0x01, s("agent")),
            f(0x02, s("agent-alpha")),
            f(
                0x03,
                rec(vec![f(
                    0x02,
                    rec(vec![
                        f(0x01, s("anthropic")),
                        f(0x02, s("claude-sonnet-4")),
                        f(0x03, s("zed")),
                        f(0x04, arr(vec![s("read"), s("edit"), s("run-test")])),
                    ]),
                )]),
            ),
            f(0x04, s("DIGEST_ONLY")),
            f(0x06, i(TS)),
        ],
    )
}

fn build_producer_git_import(_: &mut FixtureCtx) -> Object {
    obj(
        Family::Producer,
        vec![
            f(0x01, s("git_import")),
            f(0x02, s("git-import")),
            f(0x04, s("DIGEST_ONLY")),
            f(0x06, i(TS)),
        ],
    )
}

fn build_environment_linux(_: &mut FixtureCtx) -> Object {
    obj(
        Family::Environment,
        vec![
            f(
                0x01,
                rec(vec![
                    f(0x01, s("linux")),
                    f(0x02, s("ubuntu")),
                    f(0x03, s("24.04")),
                    f(0x04, s("6.8")),
                ]),
            ),
            f(0x02, s("x86_64")),
            f(0x03, rec(vec![f(0x01, s("gcc")), f(0x02, s("13.2"))])),
            f(0x04, rec(vec![f(0x01, s("rust")), f(0x02, s("1.80"))])),
            f(
                0x05,
                arr(vec![rec(vec![
                    f(0x01, s("cargo")),
                    f(0x02, s("1.80")),
                    f(0x03, bytes(&[0x01; 32])),
                ])]),
            ),
            f(
                0x06,
                rec(vec![
                    f(0x01, s("x86_64")),
                    f(0x02, u(16)),
                    f(0x03, u(68_719_476_736)),
                ]),
            ),
            f(0x08, s("full")),
            f(0x0A, s("fully_deterministic")),
            f(0x0B, i(TS)),
        ],
    )
}

fn tree_entry(name: &str, mode: u64, target: Gid) -> Value {
    rec(vec![f(0x01, s(name)), f(0x02, u(mode)), f(0x03, g(target))])
}

fn build_tree_sub(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Tree,
        vec![f(
            0x01,
            arr(vec![tree_entry("lib.rs", 0o100644, ctx.gid("blob-lib"))]),
        )],
    )
}

fn build_tree_single(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Tree,
        vec![f(
            0x01,
            arr(vec![tree_entry("a.txt", 0o100644, ctx.gid("blob-hello"))]),
        )],
    )
}

fn build_tree_mixed(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Tree,
        vec![f(
            0x01,
            arr(vec![
                tree_entry("link", 0o120000, ctx.gid("blob-link")),
                tree_entry("run.sh", 0o100755, ctx.gid("blob-script")),
                tree_entry("sub", 0o040000, ctx.gid("tree-sub")),
            ]),
        )],
    )
}

fn build_tree_after(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Tree,
        vec![f(
            0x01,
            arr(vec![
                tree_entry("a.txt", 0o100644, ctx.gid("blob-hello")),
                tree_entry("b.txt", 0o100644, ctx.gid("blob-lib")),
            ]),
        )],
    )
}

fn build_tree_final(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Tree,
        vec![f(
            0x01,
            arr(vec![
                tree_entry("a.txt", 0o100644, ctx.gid("blob-lib")),
                tree_entry("b.txt", 0o100644, ctx.gid("blob-lib")),
            ]),
        )],
    )
}

fn build_state_basic(ctx: &mut FixtureCtx) -> Object {
    obj(Family::State, vec![f(0x01, g(ctx.gid("tree-mixed")))])
}

fn build_state_after(ctx: &mut FixtureCtx) -> Object {
    obj(Family::State, vec![f(0x01, g(ctx.gid("tree-after")))])
}

fn build_state_final(ctx: &mut FixtureCtx) -> Object {
    obj(Family::State, vec![f(0x01, g(ctx.gid("tree-final")))])
}

fn build_intent_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Intent,
        vec![
            f(
                0x01,
                s("Implement pointer-loop detection matching upstream behavior"),
            ),
            f(
                0x02,
                s("Compressed DNS names must reject pointer loops exactly as BIND 9.20 does."),
            ),
            f(
                0x03,
                arr(vec![
                    s("rejects pointer loops > 16"),
                    s("matches BIND 9.20 oracle on corpus"),
                ]),
            ),
            f(0x04, arr(vec![s("no unsafe code")])),
            f(0x05, arr(vec![s("parser::decode_name")])),
            f(0x06, arr(vec![s("serializer")])),
            f(0x07, arr(vec![g(ctx.gid("state-basic"))])),
            f(0x0B, g(ctx.gid("producer-human"))),
            f(0x0C, i(TS)),
        ],
    )
}

fn build_operation_create_file(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Operation,
        vec![
            f(0x01, s("create_file")),
            f(0x02, s("b.txt")),
            f(0x05, arr(vec![g(ctx.gid("blob-lib"))])),
            f(0x06, rec(vec![f(0x01, s("ok"))])),
            f(0x07, g(ctx.gid("producer-human"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x09, i(TS)),
            f(0x0A, i(TS)),
            f(0x0B, s("create b.txt")),
            f(0x11, g(ctx.gid("blob-lib"))),
        ],
    )
}

fn build_operation_write_range(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Operation,
        vec![
            f(0x01, s("write_range")),
            f(0x02, s("a.txt")),
            f(0x04, arr(vec![g(ctx.gid("blob-hello"))])),
            f(0x05, arr(vec![g(ctx.gid("blob-lib"))])),
            f(0x06, rec(vec![f(0x01, s("ok"))])),
            f(0x07, g(ctx.gid("producer-human"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x09, i(TS)),
            f(0x0A, i(TS)),
            f(0x0B, s("replace first two bytes of a.txt")),
            f(0x12, u(0)),
            f(0x13, u(2)),
            f(0x14, g(ctx.gid("blob-lib"))),
            f(0x15, g(ctx.gid("blob-hello"))),
        ],
    )
}

fn build_operation_exec_command(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Operation,
        vec![
            f(0x01, s("exec_command")),
            f(0x06, rec(vec![f(0x01, s("ok")), f(0x03, i(0))])),
            f(0x07, g(ctx.gid("producer-agent"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x09, i(TS)),
            f(0x0A, i(TS)),
            f(0x0B, s("run unit tests")),
            f(0x1A, arr(vec![s("cargo"), s("test"), s("--lib")])),
            f(0x1B, s(".")),
            f(0x1D, g(ctx.gid("blob-lib"))),
            f(0x1E, g(ctx.gid("blob-empty"))),
        ],
    )
}

fn build_operation_run_test(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Operation,
        vec![
            f(0x01, s("run_test")),
            f(0x06, rec(vec![f(0x01, s("ok"))])),
            f(0x07, g(ctx.gid("producer-agent"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x1F, s("cargo test --lib")),
            f(0x20, arr(vec![s("decode")])),
            f(0x21, s("cargo")),
        ],
    )
}

fn build_operation_invoke_oracle(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Operation,
        vec![
            f(0x01, s("invoke_oracle")),
            f(0x06, rec(vec![f(0x01, s("ok"))])),
            f(0x07, g(ctx.gid("producer-agent"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x22, s("bind-9.20")),
            f(0x23, s("9.20.0")),
            f(0x24, g(ctx.gid("blob-lib"))),
            f(0x25, g(ctx.gid("blob-binary"))),
        ],
    )
}

fn build_episode_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Episode,
        vec![
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-basic"))),
            f(
                0x04,
                arr(vec![
                    g(ctx.gid("operation-create-file")),
                    g(ctx.gid("operation-write-range")),
                ]),
            ),
            f(0x05, g(ctx.gid("state-after"))),
            f(0x06, g(ctx.gid("producer-human"))),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(0x09, s("add b.txt and patch a.txt")),
            f(0x0A, s("completed")),
            f(0x0B, i(TS)),
            f(0x0C, i(TS)),
        ],
    )
}

fn build_evidence_oracle(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Evidence,
        vec![
            f(0x01, g(ctx.gid("producer-agent"))),
            f(0x02, s("oracle_comparison")),
            f(0x03, s("parser::decode_name")),
            f(0x05, s("frf court oracle bind-9.20 --corpus dns-2024")),
            f(0x07, arr(vec![g(ctx.gid("blob-lib"))])),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(
                0x09,
                arr(vec![rec(vec![
                    f(0x01, s("frf-court")),
                    f(0x02, s("1.2.0")),
                    f(0x03, bytes(&[0x02; 32])),
                ])]),
            ),
            f(
                0x0B,
                arr(vec![rec(vec![
                    f(0x01, s("bind-normalize")),
                    f(0x02, bytes(&[0x03; 32])),
                ])]),
            ),
            f(
                0x0C,
                arr(vec![rec(vec![
                    f(0x01, s("byte-compare")),
                    f(0x02, bytes(&[0x04; 32])),
                ])]),
            ),
            f(
                0x0D,
                rec(vec![
                    f(0x01, s("mismatch")),
                    f(0x02, s("case 441 diverges")),
                    f(
                        0x04,
                        rec(vec![
                            f(0x01, u(99421)),
                            f(0x02, u(1)),
                            f(0x03, u(0)),
                            f(0x04, u(99422)),
                        ]),
                    ),
                ]),
            ),
            f(0x0E, arr(vec![g(ctx.gid("blob-binary"))])),
            f(
                0x0F,
                rec(vec![
                    f(0x01, b(true)),
                    f(0x02, b(true)),
                    f(0x03, b(false)),
                    f(0x04, b(false)),
                ]),
            ),
            f(0x10, i(TS)),
            f(0x11, g(ctx.gid("state-after"))),
        ],
    )
}

fn build_evidence_court(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Evidence,
        vec![
            f(0x01, g(ctx.gid("producer-agent"))),
            f(0x02, s("court_receipt")),
            f(0x03, s("parser::decode_name")),
            f(
                0x05,
                s("frf court run parser-decode-name --corpus dns-2024"),
            ),
            f(0x08, g(ctx.gid("environment-linux"))),
            f(
                0x09,
                arr(vec![rec(vec![
                    f(0x01, s("frf-court")),
                    f(0x02, s("1.2.0")),
                    f(0x03, bytes(&[0x02; 32])),
                ])]),
            ),
            f(
                0x0D,
                rec(vec![
                    f(0x01, s("pass")),
                    f(
                        0x04,
                        rec(vec![
                            f(0x01, u(99421)),
                            f(0x02, u(0)),
                            f(0x03, u(0)),
                            f(0x04, u(99421)),
                        ]),
                    ),
                ]),
            ),
            f(0x0E, arr(vec![g(ctx.gid("blob-binary"))])),
            f(
                0x0F,
                rec(vec![f(0x01, b(true)), f(0x02, b(true)), f(0x04, b(false))]),
            ),
            f(0x10, i(TS)),
            f(0x11, g(ctx.gid("state-after"))),
        ],
    )
}

fn build_claim_supported(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Claim,
        vec![
            f(0x01, s("parser::decode_name")),
            f(0x03, s("parser accepts all valid RFC inputs")),
            f(0x04, s("correctness")),
            f(0x07, g(ctx.gid("producer-agent"))),
            f(0x08, arr(vec![g(ctx.gid("evidence-court"))])),
            f(
                0x0D,
                s("All 99,421 corpus cases decode without error on Linux"),
            ),
            f(0x0E, i(TS)),
        ],
    )
}

fn build_claim_contradicted(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Claim,
        vec![
            f(0x01, s("parser::decode_name")),
            f(0x03, s("parser matches BIND 9.20 on all inputs")),
            f(0x04, s("compatibility")),
            f(0x07, g(ctx.gid("producer-agent"))),
            f(0x08, arr(vec![g(ctx.gid("evidence-oracle"))])),
            f(0x0D, s("FreeBSD oracle case 441 diverges")),
            f(0x0E, i(TS)),
        ],
    )
}

fn build_residual_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Residual,
        vec![
            f(0x02, s("FreeBSD diverges from BIND oracle on case 441")),
            f(0x03, s("platform_divergence")),
            f(0x04, s("high")),
            f(
                0x05,
                rec(vec![
                    f(0x01, g(ctx.gid("intent-basic"))),
                    f(0x03, arr(vec![s("parser::decode_name")])),
                ]),
            ),
            f(0x06, arr(vec![g(ctx.gid("claim-contradicted"))])),
            f(0x08, g(ctx.gid("evidence-oracle"))),
            f(0x09, i(TS)),
            f(0x0C, i(TS)),
        ],
    )
}

fn build_verification_multi_platform(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Verification,
        vec![
            f(0x01, s("parser::decode_name")),
            f(
                0x03,
                rec(vec![
                    f(
                        0x01,
                        arr(vec![
                            rec(vec![f(0x01, s("linux")), f(0x02, s("x86_64"))]),
                            rec(vec![f(0x01, s("linux")), f(0x02, s("aarch64"))]),
                            rec(vec![f(0x01, s("freebsd")), f(0x02, s("x86_64"))]),
                        ]),
                    ),
                    f(0x02, arr(vec![s("release")])),
                    f(
                        0x03,
                        arr(vec![rec(vec![
                            f(0x01, s("frf-court")),
                            f(0x02, s("1.2.0")),
                        ])]),
                    ),
                ]),
            ),
            f(
                0x04,
                arr(vec![
                    g(ctx.gid("claim-supported")),
                    g(ctx.gid("claim-contradicted")),
                ]),
            ),
            f(
                0x05,
                arr(vec![
                    g(ctx.gid("evidence-court")),
                    g(ctx.gid("evidence-oracle")),
                ]),
            ),
            f(0x06, arr(vec![g(ctx.gid("residual-basic"))])),
            f(0x07, s("partial")),
            f(0x08, g(ctx.gid("producer-agent"))),
            f(0x09, g(ctx.gid("environment-linux"))),
            f(0x0A, i(TS)),
            f(0x0B, i(TS)),
        ],
    )
}

fn build_change_simple(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Change,
        vec![
            f(0x01, s("Add b.txt")),
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-basic"))),
            f(0x04, arr(vec![g(ctx.gid("operation-create-file"))])),
            f(0x05, g(ctx.gid("state-after"))),
            f(0x06, g(ctx.gid("producer-human"))),
            f(0x0B, g(ctx.gid("environment-linux"))),
            f(0x15, i(TS)),
        ],
    )
}

fn build_change_demo(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Change,
        vec![
            f(0x01, s("Fix parser compatibility problem")),
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-after"))),
            f(
                0x04,
                arr(vec![
                    g(ctx.gid("operation-write-range")),
                    g(ctx.gid("operation-invoke-oracle")),
                ]),
            ),
            f(0x05, g(ctx.gid("state-final"))),
            f(0x06, g(ctx.gid("producer-agent"))),
            f(0x09, s("DIGEST_ONLY")),
            f(0x0A, bytes(&[0xAB; 32])),
            f(0x0B, g(ctx.gid("environment-linux"))),
            f(
                0x0C,
                arr(vec![
                    g(ctx.gid("claim-supported")),
                    g(ctx.gid("claim-contradicted")),
                ]),
            ),
            f(
                0x0D,
                arr(vec![
                    g(ctx.gid("evidence-court")),
                    g(ctx.gid("evidence-oracle")),
                ]),
            ),
            f(0x0E, arr(vec![g(ctx.gid("residual-basic"))])),
            f(0x0F, arr(vec![g(ctx.gid("verification-multi-platform"))])),
            f(0x11, arr(vec![g(ctx.gid("change-simple"))])),
            f(0x15, i(TS)),
        ],
    )
}

fn build_change_reconciled(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Change,
        vec![
            f(0x01, s("Reconcile agent direction over baseline")),
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-basic"))),
            f(0x05, g(ctx.gid("state-final"))),
            f(0x06, g(ctx.gid("producer-human"))),
            f(0x09, s("FULL")),
            f(
                0x11,
                arr(vec![g(ctx.gid("change-simple")), g(ctx.gid("change-demo"))]),
            ),
            f(0x15, i(TS)),
        ],
    )
}

fn build_trajectory_simple(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Trajectory,
        vec![
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-basic"))),
            f(0x04, g(ctx.gid("producer-human"))),
            f(0x06, arr(vec![g(ctx.gid("change-simple"))])),
            f(0x0D, i(TS)),
            f(0x0E, i(TS)),
        ],
    )
}

fn build_trajectory_with_handoff(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Trajectory,
        vec![
            f(0x01, g(ctx.gid("trajectory-simple"))),
            f(0x02, g(ctx.gid("intent-basic"))),
            f(0x03, g(ctx.gid("state-basic"))),
            f(0x04, g(ctx.gid("producer-agent"))),
            f(0x06, arr(vec![g(ctx.gid("change-demo"))])),
            f(0x07, arr(vec![g(ctx.gid("episode-basic"))])),
            f(
                0x08,
                arr(vec![
                    g(ctx.gid("evidence-court")),
                    g(ctx.gid("evidence-oracle")),
                ]),
            ),
            f(0x09, arr(vec![g(ctx.gid("residual-basic"))])),
            f(0x0A, s("interrupted")),
            f(0x0B, s("context limit reached")),
            f(
                0x0C,
                rec(vec![
                    f(0x01, s("parser rewrite done; FreeBSD verification missing")),
                    f(0x02, arr(vec![s("parser rewrite"), s("18 tests")])),
                    f(
                        0x03,
                        arr(vec![
                            s("FreeBSD verification"),
                            s("benchmark investigation"),
                        ]),
                    ),
                    f(0x04, arr(vec![g(ctx.gid("residual-basic"))])),
                    f(
                        0x05,
                        arr(vec![
                            g(ctx.gid("evidence-court")),
                            g(ctx.gid("evidence-oracle")),
                        ]),
                    ),
                    f(
                        0x06,
                        arr(vec![
                            g(ctx.gid("change-demo")),
                            g(ctx.gid("trajectory-simple")),
                        ]),
                    ),
                    f(
                        0x07,
                        arr(vec![
                            s("run frf court on freebsd"),
                            s("investigate benchmark E82"),
                        ]),
                    ),
                ]),
            ),
            f(0x0D, i(TS)),
            f(0x0E, i(TS)),
        ],
    )
}

fn build_case_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Case,
        vec![
            f(
                0x02,
                s("Achieve BIND-compatible compressed DNS-name behavior"),
            ),
            f(0x03, g(ctx.gid("intent-basic"))),
            f(0x05, s("active")),
            f(
                0x06,
                arr(vec![
                    g(ctx.gid("change-simple")),
                    g(ctx.gid("change-demo")),
                    g(ctx.gid("change-reconciled")),
                ]),
            ),
            f(
                0x07,
                arr(vec![
                    g(ctx.gid("trajectory-simple")),
                    g(ctx.gid("trajectory-with-handoff")),
                ]),
            ),
            f(0x09, g(ctx.gid("producer-human"))),
            f(0x0A, i(TS)),
            f(0x0B, i(TS)),
        ],
    )
}

fn build_context_manifest_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::ContextManifest,
        vec![
            f(
                0x01,
                arr(vec![g(ctx.gid("state-basic")), g(ctx.gid("blob-lib"))]),
            ),
            f(0x02, arr(vec![g(ctx.gid("blob-lib"))])),
            f(
                0x03,
                arr(vec![
                    g(ctx.gid("claim-supported")),
                    g(ctx.gid("claim-contradicted")),
                ]),
            ),
            f(0x04, arr(vec![g(ctx.gid("residual-basic"))])),
            f(0x05, arr(vec![g(ctx.gid("trajectory-simple"))])),
            f(0x07, arr(vec![g(ctx.gid("blob-binary"))])),
            f(0x09, g(ctx.gid("producer-human"))),
            f(0x0B, bytes(&[0x11; 32])),
            f(0x0C, i(TS)),
        ],
    )
}

fn build_agentrun_full(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::AgentRun,
        vec![
            f(0x01, g(ctx.gid("producer-agent"))),
            f(0x02, s("anthropic")),
            f(0x03, s("claude-sonnet-4")),
            f(0x04, s("zed-agent")),
            f(
                0x05,
                arr(vec![s("read"), s("edit"), s("run-test"), s("run-frf")]),
            ),
            f(0x06, g(ctx.gid("state-after"))),
            f(0x07, g(ctx.gid("intent-basic"))),
            f(0x08, g(ctx.gid("context-manifest-basic"))),
            f(0x09, bytes(&[0x11; 32])),
            f(
                0x0A,
                arr(vec![rec(vec![
                    f(0x01, s("frf-court")),
                    f(0x02, s("1.2.0")),
                    f(0x03, bytes(&[0x02; 32])),
                ])]),
            ),
            f(0x0B, g(ctx.gid("environment-linux"))),
            f(0x0D, g(ctx.gid("trajectory-with-handoff"))),
            f(0x0E, s("DIGEST_ONLY")),
            f(0x0F, g(ctx.gid("blob-binary"))),
            f(0x10, i(TS)),
            f(0x11, i(TS)),
        ],
    )
}

fn build_reconciliation_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Reconciliation,
        vec![
            f(
                0x01,
                s("Adopt agent-demo direction; preserve baseline as engineering knowledge"),
            ),
            f(0x02, g(ctx.gid("intent-basic"))),
            f(
                0x03,
                arr(vec![
                    g(ctx.gid("trajectory-simple")),
                    g(ctx.gid("trajectory-with-handoff")),
                ]),
            ),
            f(
                0x04,
                arr(vec![g(ctx.gid("state-basic")), g(ctx.gid("state-final"))]),
            ),
            f(0x05, arr(vec![g(ctx.gid("change-demo"))])),
            f(0x06, arr(vec![g(ctx.gid("change-simple"))])),
            f(0x07, arr(vec![g(ctx.gid("residual-basic"))])),
            f(
                0x09,
                arr(vec![rec(vec![
                    f(0x01, s("dependency")),
                    f(0x02, s("possible")),
                    f(0x03, arr(vec![g(ctx.gid("change-demo"))])),
                    f(0x04, s("low")),
                    f(
                        0x05,
                        s("serialize depends on invariant changed by normalize"),
                    ),
                ])]),
            ),
            f(0x0A, arr(vec![g(ctx.gid("claim-supported"))])),
            f(0x0B, arr(vec![g(ctx.gid("claim-contradicted"))])),
            f(
                0x0C,
                arr(vec![
                    g(ctx.gid("evidence-court")),
                    g(ctx.gid("evidence-oracle")),
                ]),
            ),
            f(0x0D, arr(vec![g(ctx.gid("verification-multi-platform"))])),
            f(0x0E, g(ctx.gid("state-final"))),
            f(0x0F, g(ctx.gid("change-reconciled"))),
            f(
                0x10,
                s("FreeBSD divergence tracked as residual; oracle parity accepted for 9.20"),
            ),
            f(0x11, g(ctx.gid("producer-human"))),
            f(0x12, i(TS)),
        ],
    )
}

fn build_release_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Release,
        vec![
            f(0x01, s("gemel-0.1")),
            f(0x02, s("0.1.0")),
            f(0x03, g(ctx.gid("state-final"))),
            f(
                0x04,
                arr(vec![
                    g(ctx.gid("change-simple")),
                    g(ctx.gid("change-demo")),
                    g(ctx.gid("change-reconciled")),
                ]),
            ),
            f(0x05, arr(vec![g(ctx.gid("case-basic"))])),
            f(0x06, arr(vec![g(ctx.gid("claim-supported"))])),
            f(0x07, arr(vec![g(ctx.gid("residual-basic"))])),
            f(0x08, arr(vec![g(ctx.gid("verification-multi-platform"))])),
            f(
                0x09,
                arr(vec![rec(vec![
                    f(0x01, s("gemel")),
                    f(0x02, bytes(&[0xDD; 32])),
                    f(0x03, s("oci://registry.invalid/gemel:0.1.0")),
                ])]),
            ),
            f(0x0A, g(ctx.gid("producer-human"))),
            f(0x0B, i(TS)),
        ],
    )
}

fn build_checkpoint_basic(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Checkpoint,
        vec![
            f(0x02, s("Continuation: resolve FreeBSD divergence")),
            f(0x03, g(ctx.gid("intent-basic"))),
            f(0x04, g(ctx.gid("trajectory-with-handoff"))),
            f(0x05, g(ctx.gid("state-final"))),
            f(0x06, arr(vec![g(ctx.gid("claim-contradicted"))])),
            f(0x07, arr(vec![g(ctx.gid("residual-basic"))])),
            f(
                0x08,
                arr(vec![
                    g(ctx.gid("evidence-court")),
                    g(ctx.gid("evidence-oracle")),
                ]),
            ),
            f(0x09, arr(vec![g(ctx.gid("change-demo"))])),
            f(0x0A, arr(vec![g(ctx.gid("trajectory-simple"))])),
            f(
                0x0B,
                arr(vec![s("freebsd verification"), s("benchmark E82")]),
            ),
            f(0x0C, g(ctx.gid("producer-agent"))),
            f(0x0D, i(TS)),
        ],
    )
}

fn build_config_default(_: &mut FixtureCtx) -> Object {
    obj(
        Family::Config,
        vec![
            f(
                0x02,
                rec(vec![
                    f(
                        0x01,
                        arr(vec![
                            rec(vec![f(0x01, u(0)), f(0x02, s("retain_forever"))]),
                            rec(vec![f(0x01, u(1)), f(0x02, s("retain_policy"))]),
                            rec(vec![
                                f(0x01, u(2)),
                                f(0x02, s("prune_after_days")),
                                f(0x03, u(90)),
                            ]),
                            rec(vec![
                                f(0x01, u(3)),
                                f(0x02, s("prune_after_days")),
                                f(0x03, u(14)),
                            ]),
                        ]),
                    ),
                    f(0x02, s("retain")),
                ]),
            ),
            f(0x03, rec(vec![f(0x01, b(true)), f(0x02, u(7))])),
            f(0x04, s("never_auto_execute")),
            f(0x05, s("DIGEST_ONLY")),
            f(
                0x06,
                rec(vec![
                    f(0x01, u(1_073_741_824)),
                    f(0x02, u(64)),
                    f(0x03, u(1_000_000)),
                    f(0x04, u(100_000)),
                    f(0x05, u(16_777_216)),
                ]),
            ),
            f(0x07, i(TS)),
        ],
    )
}

fn build_mapping_git_commit(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Mapping,
        vec![
            f(0x01, s("git_commit")),
            f(0x02, s("9f3a1c2b4d5e6f708192a3b4c5d6e7f8091a2b3c")),
            f(0x03, g(ctx.gid("change-simple"))),
            f(
                0x04,
                rec(vec![
                    f(0x01, arr(vec![s("timestamp substituted")])),
                    f(0x03, arr(vec![])),
                ]),
            ),
            f(0x05, g(ctx.gid("producer-git-import"))),
            f(0x06, i(TS)),
        ],
    )
}

fn build_extension_change(ctx: &mut FixtureCtx) -> Object {
    obj(
        Family::Change,
        vec![
            f(0x01, s("Extension retention probe")),
            f(0x06, g(ctx.gid("producer-human"))),
            // Extension tag 0x80 with an opaque STRING-encoded value ("hi!").
            Field::new(0x80, Value::Raw(vec![0x03, 0x68, 0x69, 0x21])),
        ],
    )
}

// ---------------------------------------------------------------------------
// Fixture table (dependency order).
// ---------------------------------------------------------------------------

static FIXTURES: &[Fixture] = &[
    Fixture {
        name: "blob-empty",
        description: "empty blob",
        build: build_blob_empty,
    },
    Fixture {
        name: "blob-hello",
        description: "blob containing the bytes 0x68 0x69 (worked example, OBJECT_MODEL.md §1.8)",
        build: build_blob_hello,
    },
    Fixture {
        name: "blob-binary",
        description: "blob with every byte value 0x00..=0xFF",
        build: build_blob_binary,
    },
    Fixture {
        name: "blob-link",
        description: "blob containing a symlink target path",
        build: build_blob_link,
    },
    Fixture {
        name: "blob-lib",
        description: "blob containing Rust source text",
        build: build_blob_lib,
    },
    Fixture {
        name: "blob-script",
        description: "blob containing a shell script",
        build: build_blob_script,
    },
    Fixture {
        name: "producer-human",
        description: "human producer with FULL disclosure",
        build: build_producer_human,
    },
    Fixture {
        name: "producer-agent",
        description: "agent producer with DIGEST_ONLY disclosure",
        build: build_producer_agent,
    },
    Fixture {
        name: "producer-git-import",
        description: "synthetic git import producer (no fabricated identity)",
        build: build_producer_git_import,
    },
    Fixture {
        name: "environment-linux",
        description: "Linux x86_64 environment manifest",
        build: build_environment_linux,
    },
    Fixture {
        name: "tree-sub",
        description: "tree with a single file entry",
        build: build_tree_sub,
    },
    Fixture {
        name: "tree-single",
        description: "tree with one file entry",
        build: build_tree_single,
    },
    Fixture {
        name: "tree-mixed",
        description: "tree with symlink, executable, and directory entries",
        build: build_tree_mixed,
    },
    Fixture {
        name: "tree-after",
        description: "tree after change-simple",
        build: build_tree_after,
    },
    Fixture {
        name: "tree-final",
        description: "tree after change-demo",
        build: build_tree_final,
    },
    Fixture {
        name: "state-basic",
        description: "repository state over tree-mixed",
        build: build_state_basic,
    },
    Fixture {
        name: "state-after",
        description: "repository state over tree-after",
        build: build_state_after,
    },
    Fixture {
        name: "state-final",
        description: "repository state over tree-final",
        build: build_state_final,
    },
    Fixture {
        name: "intent-basic",
        description: "intent: pointer-loop detection matching upstream",
        build: build_intent_basic,
    },
    Fixture {
        name: "operation-create-file",
        description: "create_file operation",
        build: build_operation_create_file,
    },
    Fixture {
        name: "operation-write-range",
        description: "write_range operation",
        build: build_operation_write_range,
    },
    Fixture {
        name: "operation-exec-command",
        description: "exec_command operation",
        build: build_operation_exec_command,
    },
    Fixture {
        name: "operation-run-test",
        description: "run_test operation",
        build: build_operation_run_test,
    },
    Fixture {
        name: "operation-invoke-oracle",
        description: "invoke_oracle operation",
        build: build_operation_invoke_oracle,
    },
    Fixture {
        name: "episode-basic",
        description: "episode over two operations",
        build: build_episode_basic,
    },
    Fixture {
        name: "evidence-oracle",
        description: "oracle comparison evidence with mismatch and reproduction record",
        build: build_evidence_oracle,
    },
    Fixture {
        name: "evidence-court",
        description: "FRF court receipt evidence (pass)",
        build: build_evidence_court,
    },
    Fixture {
        name: "claim-supported",
        description: "claim supported by court evidence",
        build: build_claim_supported,
    },
    Fixture {
        name: "claim-contradicted",
        description: "claim contradicted by oracle evidence",
        build: build_claim_contradicted,
    },
    Fixture {
        name: "residual-basic",
        description: "open residual: FreeBSD platform divergence",
        build: build_residual_basic,
    },
    Fixture {
        name: "verification-multi-platform",
        description: "multi-platform verification run (partial)",
        build: build_verification_multi_platform,
    },
    Fixture {
        name: "change-simple",
        description: "simple change: add b.txt",
        build: build_change_simple,
    },
    Fixture {
        name: "change-demo",
        description: "acceptance-demo change with claims, evidence, residuals, verification",
        build: build_change_demo,
    },
    Fixture {
        name: "change-reconciled",
        description: "multi-parent change (reconciliation result)",
        build: build_change_reconciled,
    },
    Fixture {
        name: "trajectory-simple",
        description: "trajectory with one change",
        build: build_trajectory_simple,
    },
    Fixture {
        name: "trajectory-with-handoff",
        description: "chained trajectory with handoff and incomplete outcome",
        build: build_trajectory_with_handoff,
    },
    Fixture {
        name: "case-basic",
        description: "engineering case aggregating changes and trajectories",
        build: build_case_basic,
    },
    Fixture {
        name: "context-manifest-basic",
        description: "content-addressed context manifest",
        build: build_context_manifest_basic,
    },
    Fixture {
        name: "agentrun-full",
        description: "agent execution identity with context manifest",
        build: build_agentrun_full,
    },
    Fixture {
        name: "reconciliation-basic",
        description: "reconciliation over two trajectories",
        build: build_reconciliation_basic,
    },
    Fixture {
        name: "release-basic",
        description: "release derived from a case",
        build: build_release_basic,
    },
    Fixture {
        name: "checkpoint-basic",
        description: "continuation checkpoint",
        build: build_checkpoint_basic,
    },
    Fixture {
        name: "config-default",
        description: "default repository configuration",
        build: build_config_default,
    },
    Fixture {
        name: "mapping-git-commit",
        description: "git commit mapping with documented loss",
        build: build_mapping_git_commit,
    },
    Fixture {
        name: "extension-change",
        description: "change with an extension field (lossless retention)",
        build: build_extension_change,
    },
];
