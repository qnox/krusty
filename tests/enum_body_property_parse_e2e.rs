//! An enum class body member property (`enum class E { A; val x = … }`) now PARSES into the AST
//! without the old `unsupported enum member` error. Backend emission of the body field + running its
//! initializer in the synthesized enum constructor is not yet implemented, so such an enum is skipped
//! at lowering (never miscompiled) — this test pins only that the front end accepts the syntax.
use super::common;

#[test]
fn enum_body_property_parses_without_error() {
    let d = common::front_end_diagnostics(
        "enum class E(val a: Int) {\n  X(3);\n  val b = a * 2\n}\nfun box(): String = \"OK\"\n",
        &[],
        None,
    );
    assert!(
        d.is_empty(),
        "enum body property should parse+check cleanly, got: {d:?}"
    );
}
