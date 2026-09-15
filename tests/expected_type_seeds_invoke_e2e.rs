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

/// Run one fixture under BOTH compilers and require the same `box()` value.
///
/// `expect_box_ok_files_with_stdlib` alone only proves krusty agrees with itself. These shapes are
/// about matching the reference compiler, so it must run the identical source.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

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
    both_compilers_box(MAIN, "companion_invoke_seeding");
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
    both_compilers_box(MAIN, "throwing_argument_seeding");
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
    both_compilers_box(MAIN, "constructor_still_infers");
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
    // Both compilers now blame the offending lambda on the same LINE. They still word the
    // mismatch differently and point at different columns — kotlinc at the lambda's result
    // expression, krusty at the lambda itself — so the exact texts are recorded rather than
    // matched loosely. Converging the wording is its own diagnostic-parity change.
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "krusty writes diagnostics to stderr"
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 10,
            column: 15,
            message: "type mismatch: inferred type is String but Int was expected".to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 10,
            column: 35,
            message: "return type mismatch: expected 'Int', actual 'String'.".to_string(),
        }]
    );
}

/// Review follow-up 1: the expectation must reach the lambda's formals BEFORE its body is typed.
///
/// `T` appears only as the lambda's PARAMETER, so nothing in the argument fixes it — and the body
/// reads `s.length`, which resolves only once the parameter is known to be `String`. A seeding that
/// arrives after the body is typed leaves `s` unresolved and the member lookup fails.
#[test]
fn a_companion_invoke_body_reads_the_expected_parameter() {
    const MAIN: &str = "class Folder<T>(val step: (T) -> Int) {\n\
\x20   fun apply(seed: T): Int = step(seed)\n\
}\n\
\n\
interface Fold<T> {\n\
\x20   companion object {\n\
\x20       operator fun <T> invoke(step: (T) -> Int): Folder<T> = Folder(step)\n\
\x20   }\n\
}\n\
\n\
fun lengths(): Folder<String> = Fold { s -> s.length }\n\
fun box(): String {\n\
\x20   if (lengths().apply(\"abcd\") != 4) return \"FAIL: apply\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "companion_invoke_body_expectation");
}

/// The same shape with a RECEIVER formal: the body spells `length` bare, so the lambda's implicit
/// receiver must already be `String` when the block is typed.
#[test]
fn a_receiver_companion_invoke_body_reads_the_expected_receiver() {
    const MAIN: &str = "class Folder<T>(val step: T.() -> Int) {\n\
\x20   fun apply(seed: T): Int = seed.step()\n\
}\n\
\n\
interface Fold<T> {\n\
\x20   companion object {\n\
\x20       operator fun <T> invoke(step: T.() -> Int): Folder<T> = Folder(step)\n\
\x20   }\n\
}\n\
\n\
fun lengths(): Folder<String> = Fold { length }\n\
fun box(): String {\n\
\x20   if (lengths().apply(\"abcde\") != 5) return \"FAIL: apply\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "receiver_companion_invoke_body_expectation");
}

/// Review follow-up 2: a SAFE call's expectation describes the nullable whole (`Folder<String>?`)
/// while the invoke's own result is `Folder<T>`. Lifting one nullable layer is what fixes `T`; a
/// safe-call layer that drops the expectation leaves `T` with no source at all.
///
/// This is the callable-PROPERTY spelling: `h?.fold()` reads `fold` under the null guard and then
/// applies the invoke convention to its value. `T` appears in no parameter, so only the expectation
/// can bind it — and `box()` then calls a `T`-taking member with a `String`, which compiles only if
/// it bound to `String` rather than collapsing to `Nothing`.
#[test]
fn a_safe_property_invoke_lifts_the_nullable_expectation() {
    const MAIN: &str = "class Folder<T>(val items: List<T>) {\n\
\x20   fun with(item: T): Folder<T> = Folder(items + item)\n\
\x20   fun joined(): String = items.joinToString(\"\")\n\
}\n\
\n\
class Fold {\n\
\x20   operator fun <T> invoke(): Folder<T> = Folder(emptyList())\n\
}\n\
\n\
class Holder(val fold: Fold)\n\
\n\
fun empty(holder: Holder?): Folder<String>? = holder?.fold()\n\
fun box(): String {\n\
\x20   if (empty(null) != null) return \"FAIL: guarded\"\n\
\x20   val built = empty(Holder(Fold())) ?: return \"FAIL: absent\"\n\
\x20   if (built.with(\"ab\").with(\"cd\").joined() != \"abcd\") return \"FAIL: joined\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "safe_property_invoke_expectation");
}

/// Lifting is idempotent when the selected invoke already returns a nullable value. Stripping the
/// outer expectation to `String` asks `T?` to match a non-null result and loses the only binding for
/// `T`; constraining the actual safe-call result `T?` against `String?` binds it correctly.
#[test]
fn a_safe_property_invoke_preserves_an_already_nullable_result() {
    const MAIN: &str = "class Fold {\n\
\x20   operator fun <T> invoke(): T? = null\n\
}\n\
class Holder(val fold: Fold)\n\
fun value(holder: Holder?): String? = holder?.fold()\n\
fun box(): String {\n\
\x20   if (value(null) != null) return \"FAIL: guarded\"\n\
\x20   return if (value(Holder(Fold())) == null) \"OK\" else \"FAIL: value\"\n\
}\n";
    both_compilers_box(MAIN, "safe_property_invoke_nullable_result");
}

/// The function-VALUE safe-call spelling `nullableCallable?.invoke(…)`. A function type carries a
/// concrete result, so the lifted expectation can only ever confirm what the value already
/// declares — the case is covered to pin that the lift changes no result and the guard still
/// yields `null`.
#[test]
fn a_safe_function_value_invoke_keeps_its_declared_result() {
    const MAIN: &str = "fun call(f: ((String) -> Int)?): Int? = f?.invoke(\"abcd\")\n\
fun box(): String {\n\
\x20   if (call(null) != null) return \"FAIL: guarded\"\n\
\x20   val length: (String) -> Int = { s -> s.length }\n\
\x20   if (call(length) != 4) return \"FAIL: applied\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "safe_function_value_invoke");
}

/// Review follow-up 3: a member EXTENSION `operator fun Recv.invoke` reached through the implicit
/// dispatch receiver. `T` is supplied only by the declared result of the enclosing function, and the
/// lambda body reads `s.length`, so the expectation must reach this selection too.
#[test]
fn a_member_extension_invoke_binds_the_expected_formal() {
    const MAIN: &str = "class Folder<T>(val step: (T) -> Int) {\n\
\x20   fun apply(seed: T): Int = step(seed)\n\
}\n\
\n\
class Registry {\n\
\x20   operator fun <T> String.invoke(step: (T) -> Int): Folder<T> = Folder(step)\n\
\n\
\x20   fun lengths(): Folder<String> = \"key\" { s -> s.length }\n\
}\n\
fun box(): String {\n\
\x20   if (Registry().lengths().apply(\"abcdefg\") != 7) return \"FAIL: apply\"\n\
\x20   return \"OK\"\n\
}\n";
    both_compilers_box(MAIN, "member_extension_invoke_expectation");
}
