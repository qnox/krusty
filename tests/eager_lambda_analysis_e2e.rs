//! Under `+EagerLambdaAnalysis` kotlinc discriminates overload candidates by the lambda argument
//! they share (`EagerLambdaResolution`): the lambda is analyzed once, each candidate whose
//! function-typed parameter its results do not fit is dropped, and a candidate that needed the
//! lambda's last expression coerced to `Unit` loses to one that did not. Without the feature the
//! same calls are ambiguous. The chosen candidate compiles exactly as kotlinc compiles it.
use super::common;

const EAGER: &str = "// LANGUAGE: +EagerLambdaAnalysis\n";

/// Require krusty to report exactly kotlinc's errors for `source`, entry for entry, as recorded
/// per Kotlin version, and answer whether kotlinc accepted it.
fn accepted_like_kotlinc(source: &str) -> bool {
    let sources = [("Main.kt", source)];
    let expected = common::recorded(|| {
        common::reference_error_ledger(&sources, &common::language_directives::kotlinc_args(source))
    });
    assert_eq!(
        common::krusty_error_ledger(&sources),
        expected,
        "krusty's ledger against kotlinc {}",
        krusty::kotlin_version::target()
    );
    expected.is_empty()
}

/// Require `source` to compile as kotlinc compiles it: each of `classes` whole, the code of each of
/// `lambda_classes`, then `box()` under both compilers. A suspend lambda class's `@Metadata`, which
/// records the lambda's function, is outside this comparison.
fn assert_compiles_like_kotlinc(
    stem: &str,
    source: &str,
    classes: &[&str],
    lambda_classes: &[&str],
) {
    assert!(
        accepted_like_kotlinc(source),
        "{stem}: kotlinc {} accepts the fixture",
        krusty::kotlin_version::target()
    );
    for class in classes {
        common::assert_class_matches_kotlinc(stem, source, class);
    }
    for class in lambda_classes {
        common::assert_class_code_matches_kotlinc(stem, source, class);
    }
    common::expect_box_same_as_kotlinc(source, &format!("{stem}Run"));
}

const SAM_AND_LAMBDA: &str = "fun unitLambdaAndTypeSam(block: () -> Unit) = \"O\"\n\
    fun unitLambdaAndTypeSam(block: Sam) = \"K\"\n\
    fun interface Sam {\n\
    \x20   fun run(): Type\n\
    }\n\
    object Type\n\
    fun box(): String {\n\
    \x20   return unitLambdaAndTypeSam { Unit } + unitLambdaAndTypeSam { Type }\n\
    }\n";

#[test]
fn a_lambda_result_selects_between_a_function_type_and_a_sam() {
    assert_compiles_like_kotlinc(
        "SamAndLambda",
        &format!("{EAGER}{SAM_AND_LAMBDA}"),
        &["SamAndLambdaKt"],
        &[],
    );
}

const SUSPEND_AND_NOT_SUSPEND: &str =
    "fun stringSuspendAndUnitNotSuspend(block: suspend () -> String) = \"O\"\n\
    fun stringSuspendAndUnitNotSuspend(block: () -> Unit) = \"K\"\n\
    fun box(): String {\n\
    \x20   return stringSuspendAndUnitNotSuspend { \"\" } + stringSuspendAndUnitNotSuspend { Unit }\n\
    }\n";

/// The `suspend` candidate takes the lambda as kotlinc's `SuspendLambda` class, even though its
/// body has no suspension point, with the lambda's `@Metadata`.
#[test]
fn a_lambda_result_selects_between_a_suspend_and_a_plain_function_type() {
    assert_compiles_like_kotlinc(
        "SuspendAndNotSuspend",
        &format!("{EAGER}{SUSPEND_AND_NOT_SUSPEND}"),
        &["SuspendAndNotSuspendKt"],
        &["SuspendAndNotSuspendKt$box$1"],
    );
}

const SUSPEND_AND_SAM: &str =
    "fun suspendUnitLambdaAndStringSam(block: suspend () -> Unit) = \"O\"\n\
    fun suspendUnitLambdaAndStringSam(block: StringSam) = \"K\"\n\
    fun interface StringSam {\n\
    \x20   fun run(): String\n\
    }\n\
    fun box(): String {\n\
    \x20   return suspendUnitLambdaAndStringSam { Unit } + suspendUnitLambdaAndStringSam { \"\" }\n\
    }\n";

#[test]
fn a_lambda_result_selects_between_a_suspend_function_type_and_a_sam() {
    assert_compiles_like_kotlinc(
        "SuspendAndSam",
        &format!("{EAGER}{SUSPEND_AND_SAM}"),
        &["SuspendAndSamKt"],
        &["SuspendAndSamKt$box$1"],
    );
}

const SUSPEND_OR_PLAIN_RESULT: &str =
    "fun stringSuspendAndUnitNotSuspend(block: suspend () -> String) = \"O\"\n\
    fun stringSuspendAndUnitNotSuspend(block: () -> Unit) = \"K\"\n\
    fun box(): String = stringSuspendAndUnitNotSuspend { \"\" }\n";

/// Without the feature, candidates that differ only in what the lambda returns stay ambiguous.
#[test]
fn without_the_feature_the_lambda_does_not_select() {
    assert!(!accepted_like_kotlinc(SUSPEND_OR_PLAIN_RESULT));
}

/// Without the feature kotlinc still accepts a function type beside a SAM: it prefers the function
/// type without analyzing the lambda.
#[test]
fn without_the_feature_a_function_type_beats_a_sam() {
    assert!(accepted_like_kotlinc(SAM_AND_LAMBDA));
}

const RESULT_RULES: &str = "object Type\n\
    @JvmName(\"j1\") fun unitOrAny(b: () -> Unit) = \"U\"\n\
    @JvmName(\"j2\") fun unitOrAny(b: () -> Any) = \"A\"\n\
    @JvmName(\"j3\") fun intOrString(b: () -> Int) = \"I\"\n\
    @JvmName(\"j4\") fun intOrString(b: () -> String) = \"S\"\n\
    @JvmName(\"j5\") fun <T> genOrUnit(b: () -> T) = \"G\"\n\
    @JvmName(\"j6\") fun genOrUnit(b: () -> Unit) = \"U\"\n\
    @JvmName(\"j7\") fun nullableRes(b: () -> String?) = \"N\"\n\
    @JvmName(\"j8\") fun nullableRes(b: () -> Int) = \"I\"\n\
    @JvmName(\"j9\") fun longOrString(b: () -> Long) = \"L\"\n\
    @JvmName(\"j10\") fun longOrString(b: () -> String) = \"S\"\n\
    @JvmName(\"j11\") fun paramSame(b: (Int) -> Int) = \"I\"\n\
    @JvmName(\"j12\") fun paramSame(b: (Int) -> String) = \"S\"\n\
    @JvmName(\"j13\") fun intOrAny(b: () -> Int) = \"I\"\n\
    @JvmName(\"j14\") fun intOrAny(b: () -> Any) = \"A\"\n\
    @JvmName(\"j15\") fun retInt(b: () -> Unit) = \"U\"\n\
    @JvmName(\"j16\") fun retInt(b: () -> Int) = \"I\"\n\
    var flag = false\n\
    fun box(): String {\n\
    \x20   var r = \"\"\n\
    \x20   r += unitOrAny { \"\" }\n\
    \x20   r += unitOrAny { Unit }\n\
    \x20   r += unitOrAny { }\n\
    \x20   r += intOrString { \"\" }\n\
    \x20   r += intOrString { 1 }\n\
    \x20   r += intOrString { return@intOrString \"\" }\n\
    \x20   r += genOrUnit { \"\" }\n\
    \x20   r += genOrUnit { Unit }\n\
    \x20   r += nullableRes { null }\n\
    \x20   r += longOrString { 1 }\n\
    \x20   r += paramSame { it }\n\
    \x20   r += intOrAny { }\n\
    \x20   r += retInt { if (flag) return@retInt; 1 }\n\
    \x20   return if (r == \"AUUSISGUNLIAU\") \"OK\" else r\n\
    }\n";

/// The lambda's last expression, an empty body and each explicit `return@label` all constrain the
/// result; an integer literal fits `Long`; a candidate that coerces the result to `Unit` loses.
/// Selection is compared through the diagnostics and the run: the facade's `@JvmName` metadata and
/// its lambdas' result types carry emission differences of their own.
#[test]
fn the_lambda_results_select_like_kotlinc() {
    assert_compiles_like_kotlinc("ResultRules", &format!("{EAGER}{RESULT_RULES}"), &[], &[]);
}

const RECEIVERS: &str = "fun interface IntSam { fun run(): Int }\n\
    class C {\n\
    \x20   fun m(b: IntSam) = \"I\"\n\
    \x20   fun m(b: () -> String) = \"S\"\n\
    \x20   fun inside(): String = m { \"\" } + m { 1 }\n\
    }\n\
    fun C.ext(b: IntSam) = \"I\"\n\
    fun C.ext(b: () -> String) = \"S\"\n\
    class K {\n\
    \x20   var r = \"\"\n\
    \x20   constructor(a: () -> Unit) { r = \"U\" }\n\
    \x20   constructor(b: IntSam) { r = \"I\" }\n\
    }\n\
    fun box(): String {\n\
    \x20   val c = C()\n\
    \x20   val r = c.m { \"\" } + c.m { 1 } + c.inside() + c.ext { \"\" } + c.ext { 1 } + K { \"\" }.r + K { 1 }.r\n\
    \x20   return if (r == \"SISISIUI\") \"OK\" else r\n\
    }\n";

/// Members, extensions and constructors discriminate the same way. Selection is compared through
/// the diagnostics and the run: the extension calls' receiver spill is an emission difference of
/// its own.
#[test]
fn members_extensions_and_constructors_select_like_kotlinc() {
    assert_compiles_like_kotlinc("Receivers", &format!("{EAGER}{RECEIVERS}"), &[], &[]);
}

const NO_CANDIDATE_FITS: &str = "@JvmName(\"a1\") fun intOrString(b: () -> Int) = \"I\"\n\
    @JvmName(\"a2\") fun intOrString(b: () -> String) = \"S\"\n\
    fun box(): String = intOrString { true }\n";

/// When the lambda fits no candidate, the first one stays and reports the mismatch.
#[test]
fn when_no_candidate_fits_the_first_reports_the_mismatch() {
    assert!(!accepted_like_kotlinc(&format!(
        "{EAGER}{NO_CANDIDATE_FITS}"
    )));
}

const GENERIC_RESULT_BOUNDS: &str = "class Inv<T>(val v: T)\n\
    @JvmName(\"g1\") fun <T : Number> g(b: () -> T) = \"N\"\n\
    @JvmName(\"g2\") fun <T : CharSequence> g(b: () -> T) = \"C\"\n\
    @JvmName(\"h1\") fun <T : Number?> h(b: () -> T) = \"N\"\n\
    @JvmName(\"h2\") fun <T : CharSequence> h(b: () -> T) = \"C\"\n\
    @JvmName(\"k1\") fun <T : Number> k(b: () -> T) = \"N\"\n\
    @JvmName(\"k2\") fun <T : CharSequence?> k(b: () -> T) = \"C\"\n\
    @JvmName(\"p1\") fun <T : Number> p(b: () -> Inv<T>) = \"N\"\n\
    @JvmName(\"p2\") fun <T : CharSequence> p(b: () -> Inv<T>) = \"C\"\n\
    @JvmName(\"q1\") fun <T : Number> q(b: () -> Inv<out T>) = \"N\"\n\
    @JvmName(\"q2\") fun <T : CharSequence> q(b: () -> Inv<out T>) = \"C\"\n\
    @JvmName(\"m1\") fun <T : Number> m(b: () -> T) = \"N\"\n\
    @JvmName(\"m2\") fun m(b: () -> String) = \"S\"\n\
    @JvmName(\"r1\") fun <T : Number> r(b: () -> T?) = \"N\"\n\
    @JvmName(\"r2\") fun <T : CharSequence> r(b: () -> T?) = \"C\"\n\
    @JvmName(\"a1\") fun <T : Any> a(b: () -> T) = \"A\"\n\
    @JvmName(\"a2\") fun <T : CharSequence> a(b: () -> T) = \"C\"\n\
    fun box(): String {\n\
    \x20   val s = g { \"s\" } + g { 1 } + h { null } + h { \"s\" } + k { null } + k { 1 } +\n\
    \x20       p { Inv(\"s\") } + p { Inv(1) } + q { Inv(\"s\") } + q { Inv(1) } + m { 1 } + m { \"s\" } +\n\
    \x20       r { \"s\" } + r { 1 } + a { \"s\" } + a { 1 }\n\
    \x20   return if (s == \"CNNCCNCNCNNSCNCA\") \"OK\" else s\n\
    }\n";

/// A lambda result constrains the candidate's own type parameters: a candidate whose declared
/// bound the solved result violates drops out, directly, through a nullable result, through an
/// invariant or projected type argument, and beside a candidate without type parameters. When
/// both bounds admit the result, the more specific candidate wins.
#[test]
fn a_lambda_result_violating_a_type_parameter_bound_eliminates_the_candidate() {
    assert_compiles_like_kotlinc(
        "GenericResultBounds",
        &format!("{EAGER}{GENERIC_RESULT_BOUNDS}"),
        &[],
        &[],
    );
}

const GENERIC_MEMBER_RESULT_BOUNDS: &str = "class C {\n\
    \x20   fun <T : Number> m(x: Int, b: () -> T) = \"N\"\n\
    \x20   fun <T : CharSequence> m(x: Long, b: () -> T) = \"C\"\n\
    \x20   fun inside(): String = m(1) { \"s\" } + m(1) { 1 }\n\
    }\n\
    fun box(): String {\n\
    \x20   val c = C()\n\
    \x20   val s = c.m(1) { \"s\" } + c.m(1) { 1 } + c.inside()\n\
    \x20   return if (s == \"CNCN\") \"OK\" else s\n\
    }\n";

/// Member overloads are told apart by their bounds before the more specific `Int` parameter is
/// preferred, and the call's final selection keeps the member the analysis kept.
#[test]
fn members_are_told_apart_by_their_bounds_before_specificity() {
    assert_compiles_like_kotlinc(
        "GenericMemberResultBounds",
        &format!("{EAGER}{GENERIC_MEMBER_RESULT_BOUNDS}"),
        &[],
        &[],
    );
}

const GENERIC_RESULT_OR_UNIT: &str = "class Inv<T>(val v: T)\n\
    @JvmName(\"u1\") fun <T : Number> u(b: () -> T) = \"N\"\n\
    @JvmName(\"u2\") fun u(b: () -> Unit) = \"U\"\n\
    @JvmName(\"v1\") fun <T : CharSequence> v(b: () -> Inv<out T>) = \"C\"\n\
    @JvmName(\"v2\") fun v(b: () -> Unit) = \"U\"\n\
    fun box(): String {\n\
    \x20   val s = u { \"s\" } + u { 1 } + v { Inv(1) } + v { Inv(\"s\") }\n\
    \x20   return if (s == \"SELECTED\") \"OK\" else s\n\
    }\n";

/// A generic candidate the lambda result fits needs no coercion to `Unit`, so it wins over a
/// `() -> Unit` candidate; one whose bound the result violates drops out and leaves the coercing
/// candidate.
#[test]
fn a_bound_violating_generic_candidate_leaves_the_unit_candidate() {
    let source = GENERIC_RESULT_OR_UNIT.replace("SELECTED", "UNUC");
    assert_compiles_like_kotlinc("GenericResultOrUnit", &format!("{EAGER}{source}"), &[], &[]);
}

/// Without the feature the lambda is not analyzed first: the `() -> Unit` candidate is chosen
/// every time.
#[test]
fn without_the_feature_the_unit_candidate_is_chosen() {
    let source = GENERIC_RESULT_OR_UNIT.replace("SELECTED", "UUUU");
    assert_compiles_like_kotlinc("GenericResultOrUnitOff", &source, &[], &[]);
}
