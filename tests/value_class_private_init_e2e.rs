//! A value class's private `<init>` has no parameter null check.
//!
//! The constructor is reached only from `box-impl`, over a carrier `constructor-impl` already
//! checked, so kotlinc emits no `checkNotNullParameter` in it and its `LineNumberTable` starts at 0.
//! krusty guarded a non-null reference carrier there.
//!
//! The constructor's line-table start is now counted from the guards the constructor actually
//! emits (each parameter's `check`), not re-derived from the field's type, so the two cannot
//! disagree.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

/// The `line N: pc` rows of the member whose header contains `marker`.
fn line_rows(disassembly: &str, marker: &str) -> Vec<String> {
    disassembly
        .lines()
        .skip_while(|line| !(line.trim_end().ends_with(';') && line.contains(marker)))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter(|line| line.trim().starts_with("line "))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_value_class_private_init_is_unguarded() {
    let Some(built) = compare_with_kotlinc_plugin(
        "ValueClassPrivateInit",
        "@JvmInline value class S(val s: String)\n",
        "S",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let init = "private S(java.lang.String)";
    let reference = method_instructions(&built.reference, init);
    assert!(!reference.is_empty(), "{}", built.reference);
    assert_eq!(method_instructions(&built.krusty, init), reference);
    assert_eq!(
        line_rows(&built.krusty, init),
        line_rows(&built.reference, init)
    );
}

#[test]
fn a_value_class_still_rejects_null_at_construction() {
    common::expect_box_ok_with_stdlib(
        "@JvmInline value class S(val s: String)\n\
         fun make(raw: String?): Any = S(raw!!)\n\
         fun box(): String {\n\
         \x20   if (make(\"a\") != S(\"a\")) return \"FAIL value\"\n\
         \x20   return try { make(null); \"FAIL no NPE\" } catch (e: NullPointerException) { \"OK\" }\n\
         }\n",
        "a value class without an init guard",
    );
}
