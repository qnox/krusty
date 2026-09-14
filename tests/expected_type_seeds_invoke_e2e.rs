//! A companion `invoke`'s type parameters are seeded from the call's EXPECTED type.
//!
//! `Lens(get = { … }, set = { … })` builds an Arrow optic whose four formals cannot all be fixed from
//! the arguments: `T` only appears as the `set` lambda's RESULT, and `B` only as one of its
//! PARAMETERS. Kotlin fixes both from the expected type at the call — the declared result of the
//! enclosing function.
//!
//! krusty inferred them from the arguments alone, so a `set` that throws pinned `T = Nothing` and `B`
//! was never bound at all:
//!
//! ```text
//! error: return type mismatch:
//!   expected 'P<String, String, A, A>', actual 'P<String, Nothing, A, B>'
//! ```
//!
//! A CONSTRUCTOR of the same shape was unaffected — only the companion-`invoke` spelling missed the
//! seeding, which is what kept this narrow.

use super::common;

/// The failing shape: an interface built through its companion `invoke`, with two formals reachable
/// only from the expected type. `S` appears solely as a lambda PARAMETER and `B` solely as the
/// second one, so neither argument's own type can fix them; the declared result does.
#[test]
fn a_companion_invoke_binds_formals_only_the_expectation_supplies() {
    const MAIN: &str = "interface P<S, T, A, B> {\n\
\x20   fun get(source: S): A\n\
\x20   fun set(source: S, focus: B): T\n\
\n\
\x20   companion object {\n\
\x20       operator fun <S, T, A, B> invoke(get: (S) -> A, set: (S, B) -> T): P<S, T, A, B> =\n\
\x20           object : P<S, T, A, B> {\n\
\x20               override fun get(source: S): A = get(source)\n\
\x20               override fun set(source: S, focus: B): T = set(source, focus)\n\
\x20           }\n\
\x20   }\n\
}\n\
\n\
fun constant(): P<String, String, Int, Int> =\n\
\x20   P(\n\
\x20       get = { _ -> 3 },\n\
\x20       set = { _, _ -> \"abc\" },\n\
\x20   )\n\
fun box(): String {\n\
\x20   val lens = constant()\n\
\x20   if (lens.get(\"xyz\") != 3) return \"FAIL: get\"\n\
\x20   if (lens.set(\"xyz\", 1) != \"abc\") return \"FAIL: set\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "companion_invoke_seeding");
}

/// The corpus shape: reached through a TYPEALIAS, with a generic enclosing function, and a `set` that
/// THROWS — so its result offers `Nothing` and only the expected type can supply `T`.
#[test]
fn a_throwing_argument_does_not_pin_a_formal_the_expectation_fixes() {
    const MAIN: &str = "interface P<S, T, A, B> {\n\
\x20   fun get(source: S): A\n\
\n\
\x20   companion object {\n\
\x20       operator fun <S, T, A, B> invoke(get: (S) -> A, set: (S, B) -> T): P<S, T, A, B> =\n\
\x20           object : P<S, T, A, B> {\n\
\x20               override fun get(source: S): A = get(source)\n\
\x20           }\n\
\x20   }\n\
}\n\
\n\
typealias L<S, A> = P<S, S, A, A>\n\
\n\
fun <A> readOnly(extract: (String) -> A): L<String, A> =\n\
\x20   P(\n\
\x20       get = { source -> extract(source) },\n\
\x20       set = { _, _ -> throw UnsupportedOperationException(\"read-only\") },\n\
\x20   )\n\
fun box(): String {\n\
\x20   if (readOnly { it.length }.get(\"abcd\") != 4) return \"FAIL: extract\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "throwing_argument_seeding");
}

/// The spelling that already worked keeps working: the same shape built by a CONSTRUCTOR.
#[test]
fn a_constructor_of_the_same_shape_still_infers() {
    const MAIN: &str = "class P<S, T, A, B>(val get: (S) -> A, val set: (S, B) -> T)\n\
\n\
fun <A> make(extract: (String) -> A): P<String, String, A, A> =\n\
\x20   P(\n\
\x20       get = { source -> extract(source) },\n\
\x20       set = { source, _ -> source },\n\
\x20   )\n\
fun box(): String {\n\
\x20   val built = make { it.length }\n\
\x20   if (built.get(\"abc\") != 3) return \"FAIL: get\"\n\
\x20   if (built.set(\"abc\", 1) != \"abc\") return \"FAIL: set\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "constructor_still_infers");
}

/// An expectation that genuinely does not fit is still rejected — seeding from it must not accept a
/// mismatched result. Both compilers' output is asserted.
#[test]
fn a_mismatched_expectation_is_still_rejected() {
    const MAIN: &str = "interface P<S, T, A, B> {\n\
\x20   companion object {\n\
\x20       operator fun <S, T, A, B> invoke(get: (S) -> A, set: (S, B) -> T): P<S, T, A, B> =\n\
\x20           object : P<S, T, A, B> {}\n\
\x20   }\n\
}\n\
\n\
fun bad(): P<String, String, Int, Int> =\n\
\x20   P(\n\
\x20       get = { source: String -> source },\n\
\x20       set = { source: String, _: Int -> source },\n\
\x20   )\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a String focus against an Int expectation: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a mismatched expectation: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
