//! An inlined body keeps its own platform-value null checks, as kotlinc does.
//!
//! kotlinc's inliner drops the inlined function's parameter checks (`checkNotNullParameter`) — the
//! arguments are the caller's own values — but keeps every `checkNotNullExpressionValue` the body
//! makes on a platform value it reads (`dup; ldc; invokestatic`), because that value comes from the
//! callee's Java call, not the caller. krusty's splice deleted both kinds.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const SOURCE: &str = "fun upper(text: String): String = text.uppercase()\n\
    fun lower(text: String): String = text.lowercase()\n";

#[test]
fn a_spliced_body_keeps_its_expression_null_checks_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "SplicedExpressionNullCheck",
        SOURCE,
        "SplicedExpressionNullCheckKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in ["java.lang.String upper(", "java.lang.String lower("] {
        let reference = method_instructions(&built.reference, member);
        assert!(
            reference
                .iter()
                .any(|insn| insn.contains("checkNotNullExpressionValue")),
            "{member}: kotlinc keeps the body's check: {reference:#?}"
        );
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

#[test]
fn a_spliced_body_with_its_null_checks_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   if (upper(\"ok\") != \"OK\") return \"upper\"\n\
             \x20   return if (lower(\"OK\") == \"ok\") \"OK\" else \"lower\"\n\
             }}\n"
        ),
        "spliced expression null check",
    );
}
