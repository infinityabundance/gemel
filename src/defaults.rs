//! Default object construction (producers, repository config).
//!
//! These builders depend only on the object layer; both the store
//! initializer and the workflow use them.

use crate::store::now_ms;
use crate::value::{Field, Object, Value};

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

/// A human producer object (OBJECT_MODEL.md §6.14).
pub fn human_producer_object(name: &str, email: Option<&str>) -> Object {
    let mut identity = vec![f(0x01, s(name))];
    if let Some(email) = email {
        identity.push(f(0x02, s(email)));
    }
    Object::fields(
        crate::family::Family::Producer,
        vec![
            f(0x01, s("human")),
            f(0x02, s(name)),
            f(0x03, rec(vec![f(0x01, rec(identity))])),
            f(0x04, s("FULL")),
            f(0x06, i(now_ms())),
        ],
    )
}

/// An automation producer object (OBJECT_MODEL.md §6.14).
pub fn automation_producer_object(name: &str) -> Object {
    Object::fields(
        crate::family::Family::Producer,
        vec![
            f(0x01, s("automation")),
            f(0x02, s(name)),
            f(
                0x03,
                rec(vec![f(
                    0x03,
                    rec(vec![f(0x01, s(name)), f(0x02, s("0.1.0"))]),
                )]),
            ),
            f(0x04, s("DIGEST_ONLY")),
            f(0x06, i(now_ms())),
        ],
    )
}

/// The default repository configuration object (OBJECT_MODEL.md §6.21).
///
/// GC is declared but not yet executed by any runner: the config is
/// declarative policy; the GC pass arrives in a later phase.
pub fn default_config_object() -> Object {
    Object::fields(
        crate::family::Family::Config,
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
            // GC policy is declared; no GC runner exists yet (Phase 2+).
            f(0x03, rec(vec![f(0x01, b(false)), f(0x02, u(7))])),
            f(0x04, s("never_auto_execute")),
            f(0x05, s("DIGEST_ONLY")),
            f(
                0x06,
                rec(vec![
                    f(0x01, u(1 << 30)),
                    f(0x02, u(64)),
                    f(0x03, u(1_000_000)),
                    f(0x04, u(100_000)),
                    f(0x05, u(16 << 20)),
                ]),
            ),
            f(0x07, i(now_ms())),
        ],
    )
}
