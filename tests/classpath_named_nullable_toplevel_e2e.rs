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
