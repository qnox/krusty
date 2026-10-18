//! kotlinc's reified type-argument checks (`FirReifiedChecker`): a type argument for a `reified`
//! type parameter, or for `kotlin.Array`'s constructor, must be a runtime-available type. A
//! non-reified type parameter, `Nothing`, a definitely-non-nullable type written explicitly, and an
//! inferred intersection are reported at the explicit type argument, else at the callee name.
//!
//! Reached only through a definitely-non-nullable or flexible type, a type parameter or an
//! intersection is a KTLC-399 deprecation warning until `ReportReificationProblemsInDnnAndFlexible`
//! makes it the ordinary error. kotlinc's CLI prints no warnings for a file with errors, so each
//! warning shape is compiled on its own.

use super::common;

const DNN_AND_FLEXIBLE: &str = "-XXLanguage:+ReportReificationProblemsInDnnAndFlexible";

const PRELUDE: &str = "\
inline fun <reified T> ra(x: T): T = x
inline fun <reified T> r(): String = T::class.java.name
";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and `warnings`.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[&str], warnings: &[&str]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Reified.kt", source)],
        &[common::stdlib_jar()],
        &args,
    );
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let krusty_errors = common::compiler_errors(&krusty_output);
    let kotlinc_errors = common::compiler_errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors, "errors\n{krusty_output}");
    let krusty_warnings = common::compiler_warnings(&krusty_output);
    let kotlinc_warnings = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(
        krusty_warnings, kotlinc_warnings,
        "warnings\n{krusty_output}"
    );
    let render = |diagnostics: &[common::CompilerError]| {
        diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{}:{}: {}",
                    diagnostic.line, diagnostic.column, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(render(&kotlinc_errors), errors);
    assert_eq!(render(&kotlinc_warnings), warnings);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

fn with_prelude(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

const AS_REIFIED: &str = "cannot use 'T' as reified type parameter. Use a class instead.";
const AS_REIFIED_ARRAY: &str =
    "cannot use 'T' as reified type parameter, since the array type parameter is not reified.";
const NOTHING: &str = "cannot use 'Nothing' as reified type parameter.";
const NULLABLE_NOTHING: &str = "cannot use 'Nothing?' as reified type parameter.";
const DNN: &str = "cannot use definitely-non-nullable type as a reified type argument.";
const DNN_WARNING: &str = "cannot use 'T' as reified type parameter. Use a class instead. This \
                           will become an error in language version 2.6. See \
                           https://youtrack.jetbrains.com/issue/KTLC-399.";

fn intersection(types: &str) -> String {
    format!(
        "type argument for reified type parameter 'T' was inferred to the intersection of \
         [{types}]. Reification of an intersection type results in the common supertype being \
         used. This may lead to subtle issues and an explicit type argument is encouraged."
    )
}

const SHAPES: &str = "\
typealias Arr<X> = Array<X>
interface I1
interface I2
class C12 : I1, I2
class D12 : I1, I2
fun <T> f1() = r<T>()
fun <T> f2() = r<Array<T>>()
fun <T> f3() = r<Arr<T>>()
fun <T> f4() = r<Array<T>?>()
fun <T> f5(x: Array<T>) = ra(x)
fun <T> f6() = r<Array<out T>>()
fun f7() = r<Array<*>>()
fun f8() = r<Nothing?>()
fun f9() = ra(null)
fun f10() = r<Nothing>()
fun <T> f13() = r<T & Any>()
fun f14(b: Boolean) = ra(if (b) C12() else D12())
fun f15(b: Boolean) = ra(if (b) 1 else \"\")
fun <T> f16() = arrayOf<T>()
fun <T> f17(x: T) = arrayOf(x, x)
fun <T> f18() = emptyArray<T>()
fun <T> f19(n: Int, x: T) = Array<T>(n) { x }
fun <T> f20(n: Int, x: T) = Array(n) { x }
fun <T> f21(x: T) = listOf(x).toTypedArray()
fun <T> f22(l: java.util.ArrayList<T>) = ra(l.get(0))
fun <T> f23(x: T?) = ra(x)
fun <T> f24() = r<T?>()
inline fun <reified T> f26() = r<T>() + r<Array<T>>()
fun <T> f27(x: T) = mutableListOf(x)
class G<T> { fun g() = r<T>() }
fun <T> f28() = r<List<T>>()
fun <T> f29() = r<Array<Array<T>>>()
fun <T> f30() = r<Array<in T>>()
fun <T : Any> f31(l: java.util.ArrayList<T>) = ra(l.get(0))
fun <T> f32(x: T & Any) = r<Array<T & Any>>()
";

fn shape_errors(dnn_and_flexible: bool) -> Vec<String> {
    let mut errors = vec![
        format!("8:18: {AS_REIFIED}"),
        format!("9:18: {AS_REIFIED_ARRAY}"),
        format!("10:18: {AS_REIFIED_ARRAY}"),
        format!("11:18: {AS_REIFIED_ARRAY}"),
        format!("12:27: {AS_REIFIED_ARRAY}"),
        format!("17:15: {NOTHING}"),
        format!("18:19: {DNN}"),
    ];
    if dnn_and_flexible {
        errors.push(format!("18:19: {AS_REIFIED}"));
    }
    errors.extend([
        format!("19:23: {}", intersection("'I1' & 'I2'")),
        format!(
            "20:23: {}",
            intersection("'Comparable<*>' & 'Serializable'")
        ),
        format!("21:25: {AS_REIFIED}"),
        format!("22:21: {AS_REIFIED}"),
        format!("23:28: {AS_REIFIED}"),
        format!("24:35: {AS_REIFIED}"),
        format!("25:29: {AS_REIFIED}"),
        format!("26:31: {AS_REIFIED}"),
        format!("27:42: {AS_REIFIED}"),
        format!("28:22: {AS_REIFIED}"),
        format!("29:19: {AS_REIFIED}"),
        format!("32:26: {AS_REIFIED}"),
        format!("34:19: {AS_REIFIED_ARRAY}"),
        format!("36:48: {AS_REIFIED}"),
        format!("37:29: {DNN}"),
    ]);
    if dnn_and_flexible {
        errors.push(format!("37:29: {AS_REIFIED_ARRAY}"));
    }
    errors
}

const CONDITIONAL_SHAPES: &str = "\
interface J1
interface J2
class CJ : J1, J2
class DJ : J1, J2
fun cif(b: Boolean) = ra(if (b) CJ() else DJ())
fun cwhen(i: Int) = ra(when (i) { 0 -> CJ(); 1 -> DJ(); else -> CJ() })
fun celvis(x: CJ?) = ra(x ?: DJ())
fun ctry(b: Boolean) = ra(try { if (b) CJ() else throw IllegalStateException() } catch (_: IllegalStateException) { DJ() })
fun cnested(a: Boolean, b: Boolean) = ra(if (a) if (b) CJ() else DJ() else CJ())
";

/// The broad overload probes the same callable-reference expression first as `(String) -> Any`,
/// where the generic `pick(T): Any` is more specific by parameter type. The more-specific overload
/// then rechecks that expression as `(String) -> String`; the generic result is inapplicable and the
/// non-generic declaration wins. No generic/reified commitment from the discarded probe belongs to
/// the final callable reference.
const CONTEXTUAL_GENERIC_TO_NON_GENERIC_REFERENCE: &str = "\
@kotlin.jvm.JvmName(\"genericPick\")
inline fun <reified T : Any> pick(value: T): Any = value
fun pick(value: Any): String = value.toString()
fun use(block: (String) -> Any, marker: Any): Int = block(marker.toString()).hashCode()
fun use(block: (String) -> String, marker: String): String = block(marker)
fun selected(): String = use(::pick, \"OK\")
";

#[test]
fn every_conditional_form_retains_exact_intersection_diagnostics() {
    let diagnostic = intersection("'J1' & 'J2'");
    assert_matches_kotlinc(
        &with_prelude(CONDITIONAL_SHAPES),
        &[],
        &[
            &format!("7:23: {diagnostic}"),
            &format!("8:21: {diagnostic}"),
            &format!("9:22: {diagnostic}"),
            &format!("10:24: {diagnostic}"),
            &format!("11:39: {diagnostic}"),
        ],
        &[],
    );
}

#[test]
fn a_contextual_non_generic_reference_replaces_the_generic_probe_commitment() {
    assert_matches_kotlinc(CONTEXTUAL_GENERIC_TO_NON_GENERIC_REFERENCE, &[], &[], &[]);
    let (diagnostics, selected_arguments) = common::inspect_checker_with_stdlib(
        CONTEXTUAL_GENERIC_TO_NON_GENERIC_REFERENCE,
        |file, info, _| {
            file.expr_arena
                .iter()
                .enumerate()
                .find_map(|(index, expression)| {
                    matches!(
                        expression,
                        krusty::ast::Expr::CallableRef { name, .. } if name == "pick"
                    )
                    .then(|| {
                        info.resolved_call_type_arguments(krusty::ast::ExprId(index as u32))
                            .map(<[_]>::to_vec)
                    })
                })
        },
    );
    assert_eq!(diagnostics, Vec::<String>::new());
    assert_eq!(
        selected_arguments,
        Some(None),
        "the selected non-generic ::pick must own no type-argument commitment"
    );
}

#[test]
fn reified_type_argument_shapes_match_kotlinc() {
    let errors = shape_errors(false);
    let errors = errors.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(&with_prelude(SHAPES), &[], &errors, &[]);
}

#[test]
fn reified_type_argument_shapes_with_dnn_and_flexible_reports_match_kotlinc() {
    let errors = shape_errors(true);
    let errors = errors.iter().map(String::as_str).collect::<Vec<_>>();
    assert_matches_kotlinc(&with_prelude(SHAPES), &[DNN_AND_FLEXIBLE], &errors, &[]);
}

/// Each shape reaches a non-reified type parameter only through a definitely-non-nullable or a
/// flexible type, with the column of its report.
const THROUGH_DNN_OR_FLEXIBLE: &[(&str, u32, bool)] = &[
    ("fun <T> f(x: T & Any) = ra(x)", 25, false),
    (
        "fun <T> f(x: T) = ra(java.util.Objects.requireNonNull(x))",
        19,
        false,
    ),
    ("fun <T> f(x: Array<T & Any>) = ra(x)", 32, true),
    (
        "fun <T> f(l: java.util.ArrayList<T>, a: Array<T>) = ra(l.toArray(a))",
        53,
        true,
    ),
    ("fun <T> f(x: T & Any) = arrayOf(x)", 25, false),
];

#[test]
fn a_type_parameter_through_dnn_or_flexible_is_a_deprecation_warning() {
    for (body, column, _) in THROUGH_DNN_OR_FLEXIBLE {
        assert_matches_kotlinc(
            &with_prelude(body),
            &[],
            &[],
            &[&format!("3:{column}: {DNN_WARNING}")],
        );
    }
}

#[test]
fn a_function_parameter_dnn_occurrence_is_a_deprecation_warning() {
    let source = with_prelude(
        "inline fun <reified X> rf(noinline block: (X) -> Unit) = Unit\n\
         fun <T> f(block: (T & Any) -> Unit) = rf(block)",
    );
    assert_matches_kotlinc(&source, &[], &[], &[&format!("4:39: {DNN_WARNING}")]);
}

#[test]
fn a_type_parameter_through_dnn_or_flexible_is_an_error_once_reported() {
    for (body, column, array) in THROUGH_DNN_OR_FLEXIBLE {
        let message = if *array { AS_REIFIED_ARRAY } else { AS_REIFIED };
        assert_matches_kotlinc(
            &with_prelude(body),
            &[DNN_AND_FLEXIBLE],
            &[&format!("3:{column}: {message}")],
            &[],
        );
    }
}

const FLEXIBLE_INTERSECTION: &str =
    "fun f(b: Boolean) = ra(java.util.Objects.requireNonNull(if (b) 1 else \"\"))";

#[test]
fn an_intersection_through_a_flexible_type_is_a_deprecation_warning() {
    let message = format!(
        "{} This will become an error in language version 2.6. See \
         https://youtrack.jetbrains.com/issue/KTLC-399.",
        intersection("'Comparable<*>' & 'Serializable'")
    );
    assert_matches_kotlinc(
        &with_prelude(FLEXIBLE_INTERSECTION),
        &[],
        &[],
        &[&format!("3:21: {message}")],
    );
    assert_matches_kotlinc(
        &with_prelude(FLEXIBLE_INTERSECTION),
        &[DNN_AND_FLEXIBLE],
        &[&format!(
            "3:21: {}",
            intersection("'Comparable<*>' & 'Serializable'")
        )],
        &[],
    );
}

#[test]
fn an_intersection_is_a_deprecation_warning_without_its_prohibition() {
    let message = format!(
        "{} This will become an error in language version 2.3. See \
         https://youtrack.jetbrains.com/issue/KTLC-13.",
        intersection("'Comparable<*>' & 'Serializable'")
    );
    assert_matches_kotlinc(
        &with_prelude("fun f(b: Boolean) = ra(if (b) 1 else \"\")"),
        &["-XXLanguage:-ProhibitIntersectionReifiedTypeParameter"],
        &[],
        &[&format!("3:21: {message}")],
    );
}

#[test]
fn nullable_nothing_is_reified_only_with_its_feature() {
    let source = with_prelude("fun f() = r<Nothing?>()\nfun g() = ra(null)");
    assert_matches_kotlinc(&source, &[], &[], &[]);
    assert_matches_kotlinc(
        &source,
        &["-XXLanguage:-NullableNothingInReifiedPosition"],
        &[
            &format!("3:13: {NULLABLE_NOTHING}"),
            &format!("4:11: {NULLABLE_NOTHING}"),
        ],
        &[],
    );
}
