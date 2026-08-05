//! A classpath top-level function with CONTEXT parameters AND a vararg, called through the
//! named-argument channel: the applicability score must use the context-STRIPPED signature's
//! `vararg_index` (the mapped slots are value-indexed). Scoring with the full signature's index
//! left the vararg slot untreated, so an element-form argument was checked against the ARRAY
//! type and the call was rejected. The admission gate also needs the metadata call sig to carry
//! parameter names ACROSS the context prefix (kotlinc keeps context params out of
//! `value_parameter`, so a full-arity name list must prepend them). kotlinc 2.4.10 accepts and
//! runs both shapes below.
use super::common;

const LIB: &str = "package lib\n\
     class C(val tag: String)\n\
     context(c: C) fun assemble(prefix: String, vararg parts: String): String =\n\
     \x20 c.tag + prefix + parts.joinToString(\"\")\n\
";

#[test]
fn element_form_vararg_after_named_arg_on_context_function_resolves() {
    let main = "import lib.C\n\
        import lib.assemble\n\
        fun probe(): String = with(C(\"k\")) { assemble(prefix = \"p\", \"a\", \"b\") }\n";
    if let Some(diags) = common::checker_diags_against("cpcontextvarargnamed", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn wrong_element_type_at_context_vararg_slot_is_rejected() {
    // The value-indexed vararg slot must reject a mistyped element the same way kotlinc does —
    // guarding against a fix that admits ANY argument at the shifted slot.
    let main = "import lib.C\n\
        import lib.assemble\n\
        fun probe(): String = with(C(\"k\")) { assemble(prefix = \"p\", 1, 2) }\n";
    if let Some(diags) = common::checker_diags_against("cpcontextvarargnamedbad", LIB, main) {
        assert!(
            !diags.is_empty(),
            "an Int element at a String vararg slot must be rejected"
        );
    }
}
