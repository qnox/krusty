//! NAMED arguments to a CLASSPATH top-level function whose parameter is `String?` (metadata-nullable):
//! `pkg.fw<Int>(message = null) { 42 }`. The named-argument mapping channel scored the slots against the
//! descriptor-erased `params` (where `String?` reads as `String`) without applying the metadata
//! nullability carried in `platform_nullable_params`, so kotlinc-clean code was rejected with
//! "null cannot be a value of a non-null type 'String'.". The positional and omitted-argument spellings
//! already widened through `apply_platform_call_parameter_nullability`.
use super::common;

const LIB: &str = "package pkg\n\
     fun <T> fw(message: String? = null, block: () -> T): T {\n\
     \x20 if (message != null) println(message)\n\
     \x20 return block()\n\
     }\n\
     fun <T> fw2(tag: Int = 0, message: String? = null, block: () -> T): T {\n\
     \x20 if (message != null) println(\"$tag $message\")\n\
     \x20 return block()\n\
     }\n";

#[test]
fn named_null_for_nullable_string_param_fq_call() {
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int>(message = null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltl", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn named_null_for_nullable_string_param_imported_call() {
    let main = "import pkg.fw\n\
        fun box(): String {\n\
        \x20 val n = fw<Int>(message = null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltlimp", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn positional_null_for_nullable_string_param_stays_clean() {
    // Regression guard: the positional spelling already worked.
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int>(null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltlpos", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn named_null_skipping_leading_defaulted_param() {
    // The named nullable arg lands in a NON-LEADING slot (slot 0 `tag` defaults away), so the slot
    // map no longer coincides with positional order — a positional params-vs-args zip would pair
    // `tag: Int` with `null` and leave `message: String?` unwidened. Only slot-driven widening
    // accepts this shape.
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw2<Int>(message = null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltlskip", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn named_null_skipping_leading_defaulted_param_imported() {
    // Imported spelling of the slot-skip shape: the bare-name channel resolves it through the
    // named `$default` path (`resolve_top_level_named_default_callable`).
    let main = "import pkg.fw2\n\
        fun box(): String {\n\
        \x20 val n = fw2<Int>(message = null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltlskipimp", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

#[test]
fn named_null_skipping_leading_defaulted_param_runs() {
    // The slot-skip shape end-to-end: the `$default` synthetic must be selected, lowered with the
    // right mask, and produce the block's value.
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw2<Int>(message = null) { 42 }\n\
        \x20 val m = pkg.fw2<Int>(tag = 7) { 8 }\n\
        \x20 return if (n == 42 && m == 8) \"OK\" else \"fail: $n $m\"\n\
        }\n";
    common::expect_box_ok_against("cpnamednulltlskiprun", LIB, main);
}

#[test]
fn named_null_for_nullable_string_param_runs() {
    // End-to-end: the resolved call must also lower and run.
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int>(message = null) { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    common::expect_box_ok_against("cpnamednulltlrun", LIB, main);
}

#[test]
fn named_non_null_string_still_resolves() {
    let main = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int>(message = \"m\") { 42 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    if let Some(diags) = common::checker_diags_against("cpnamednulltlstr", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}
