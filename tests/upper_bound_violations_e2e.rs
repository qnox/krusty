//! kotlinc's `UPPER_BOUND_VIOLATED` (`FirUpperBoundViolatedChecker`): a type argument outside its
//! parameter's declared upper bounds, substituted with the arguments of the same application. The
//! message names the parameter with its owner, numbers same-named type parameters (`T#1`, `T#2`),
//! and writes several bounds as their intersection.

use super::common;

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and warnings, which must also be `errors` and no warnings.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[String]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Bounds.kt", source)],
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
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, errors);
    assert!(kotlinc_warnings.is_empty());
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

/// `type argument is not within its bounds` for `parameter` (with its owner), `bound`, `actual`.
fn violation(position: &str, parameter: &str, bound: &str, actual: &str) -> String {
    format!(
        "{position}: type argument is not within its bounds: type parameter '{parameter}' must be \
         subtype of '{bound}', but actual: '{actual}'."
    )
}

fn b_violation(position: &str, actual: &str) -> String {
    violation(position, "T (of class B<T : Number>)", "Number", actual)
}

const DECLARATIONS: &str = "\
class B<T : Number>
class C<T : Number, U : CharSequence>
interface I<T : Comparable<T>>
interface J<X : J<X>>
interface X
interface Y
class M<T> where T : X, T : Y
object XO : X
class G<in T : Number>
val v1: B<String>? = null
val v2: C<String, Int>? = null
val v3: List<B<String>>? = null
val v4: Array<B<String>>? = null
val v5: B<*>? = null
val v6: B<out String>? = null
val v7: B<in String>? = null
val v8: B<Int?>? = null
val v9: B<Nothing>? = null
val v10: ((B<String>) -> B<String>)? = null
val v11: M<XO>? = null
val v12: Enum<String>? = null
val v13: G<Any>? = null
fun h(x: B<String>): B<Any>? = null
fun B<String>.ext() {}
class D : I<Any>
class Q : J<Q>
class R : J<Q>
abstract class P<T : Number> : List<B<T>>
abstract class P2 : Comparable<B<String>>
open class Base<T : Number>
class Sub : Base<String>()
class Ctor(val b: B<String>)
class E<T : B<String>>
class E2<T> where T : List<B<String>>
";

#[test]
fn declared_types_supertypes_and_classifier_bounds_are_checked() {
    assert_matches_kotlinc(
        DECLARATIONS,
        &[],
        &[
            b_violation("10:11", "String"),
            violation(
                "11:11",
                "T (of class C<T : Number, U : CharSequence>)",
                "Number",
                "String",
            ),
            violation(
                "11:19",
                "U (of class C<T : Number, U : CharSequence>)",
                "CharSequence",
                "Int",
            ),
            b_violation("12:16", "String"),
            b_violation("13:17", "String"),
            b_violation("15:11", "String"),
            b_violation("16:11", "String"),
            b_violation("17:11", "Int?"),
            b_violation("19:14", "String"),
            b_violation("19:28", "String"),
            violation("20:12", "T (of class M<T : X, Y>)", "X & Y", "XO"),
            violation(
                "21:15",
                "E (of class Enum<E : Enum<E>>)",
                "Enum<String>",
                "String",
            ),
            violation("22:12", "T (of class G<in T : Number>)", "Number", "Any"),
            b_violation("23:12", "String"),
            b_violation("23:24", "Any"),
            b_violation("24:7", "String"),
            violation(
                "25:13",
                "T (of interface I<T : Comparable<T>>)",
                "Comparable<Any>",
                "Any",
            ),
            b_violation("29:34", "String"),
            violation("31:18", "T (of class Base<T : Number>)", "Number", "String"),
            b_violation("32:21", "String"),
            b_violation("33:15", "String"),
            b_violation("34:30", "String"),
        ],
    );
}

const TYPE_PARAMETER_ARGUMENTS: &str = "\
class B<T : Number>
interface I<T : Comparable<T>>
interface In2<X> { fun g(x: B<X>) }
class Outer<T> { fun f(x: B<T>) {} }
fun <T> p(x: B<T>) {}
fun <T : String> q(x: B<T>) {}
fun <T, U : T> two(x: B<U>) {}
fun <T> x(a: I<T>) {}
val <T> List<T>.pp: B<T>? get() = null
class B2<T : Number> {
    fun <T> m(x: B2<T>) {}
}
";

/// A type-parameter argument names its owner, and same-named parameters are numbered in the order
/// the bound, the argument, and the checked parameter mention them.
#[test]
fn type_parameter_arguments_name_and_number_their_owners() {
    let b2 = |position: &str, actual: &str| {
        violation(position, "T#2 (of class B<T : Number>)", "Number", actual)
    };
    assert_matches_kotlinc(
        TYPE_PARAMETER_ARGUMENTS,
        &[],
        &[
            b_violation("3:31", "X (of interface In2<X>)"),
            b2("4:29", "T#1 (of class Outer<T>)"),
            b2("5:16", "T#1 (of fun <T> p)"),
            b2("6:25", "T#1 (of fun <T : String> q)"),
            b_violation("7:25", "U (of fun <T, U : T> two)"),
            violation(
                "8:16",
                "T#2 (of interface I<T : Comparable<T>>)",
                "Comparable<T#1 (of fun <T> x)>",
                "T#1 (of fun <T> x)",
            ),
            b2("9:23", "T#1 (of val <T> List<T>.pp)"),
            violation(
                "11:21",
                "T#2 (of class B2<T : Number>)",
                "Number",
                "T#1 (of fun <T> m)",
            ),
        ],
    );
}

const BODIES_AND_OPERATORS: &str = "\
class B<T : Number>
interface I<T : Comparable<T>>
fun locals() {
    val l: B<String>? = null
    val k = object : I<Any> {}
    val lam = { x: B<String> -> }
    fun <L : List<B<String>>> local() {}
    class LC<T : B<String>>
}
fun cast(a: Any) = a as B<String>
fun safeCast(a: Any) = a as? B<String>
fun nestedCast(a: Any) = a as List<B<String>>
fun test(a: Any) = a is B<String>
fun negatedTest(a: Any) = a !is B<String>
fun branch(a: Any) = when (a) { is B<String> -> 1; !is List<B<String>> -> 2; else -> 3 }
fun <T : B<String>> r() {}
fun <T> s() where T : B<String> {}
fun <T : List<B<String>>> nb() {}
val <T : B<String>> T.px: Int get() = 1
fun localBound() {
    fun <L : B<String>> local() {}
}
";

/// The outermost application of a type operator's target and of a callable's type-parameter bound
/// is not checked; the applications nested in them, and every local declaration, are.
#[test]
fn type_operators_and_callable_bounds_check_only_nested_arguments() {
    let erased = |position: &str, ty: &str| {
        format!("{position}: cannot check for instance of erased type '{ty}'.")
    };
    assert_matches_kotlinc(
        BODIES_AND_OPERATORS,
        &[],
        &[
            b_violation("4:14", "String"),
            violation(
                "5:24",
                "T (of interface I<T : Comparable<T>>)",
                "Comparable<Any>",
                "Any",
            ),
            b_violation("6:22", "String"),
            b_violation("7:21", "String"),
            b_violation("8:20", "String"),
            b_violation("12:38", "String"),
            erased("13:25", "B<String>"),
            erased("14:33", "B<String>"),
            erased("15:36", "B<String>"),
            erased("15:56", "List<B<String>>"),
            b_violation("15:63", "String"),
            b_violation("18:17", "String"),
        ],
    );
}

const BODYLESS_MEMBERS: &str = "\
class B<T : Number>
interface In {
    val p: B<String>
    fun f(x: B<String>): B<String>
    val <T> List<T>.e: B<T>
    fun B<String>.r()
}
abstract class Ab {
    abstract val q: B<String>
    abstract fun g(vararg x: B<String>)
    abstract var List<B<String>>.w: Int
}
";

/// An abstract or interface member has no body to check, but its declared types are checked
/// like any other declaration's.
#[test]
fn bodyless_members_check_their_declared_types() {
    assert_matches_kotlinc(
        BODYLESS_MEMBERS,
        &[],
        &[
            b_violation("3:14", "String"),
            b_violation("4:16", "String"),
            b_violation("4:28", "String"),
            violation(
                "5:26",
                "T#2 (of class B<T : Number>)",
                "Number",
                "T#1 (of val <T> List<T>.e)",
            ),
            b_violation("6:11", "String"),
            b_violation("9:23", "String"),
            b_violation("10:32", "String"),
            b_violation("11:25", "String"),
        ],
    );
}

/// A separately compiled module publishes the bound in Kotlin metadata. The consumer must read
/// that provider-normalized declaration rather than accepting the argument because no source AST
/// is present in this compilation.
#[test]
fn dependency_classifier_bounds_are_checked_from_metadata() {
    let dependency = common::kotlinc_lib_out(&[(
        "Dependency.kt",
        "package dependency\nclass Box<T : Number>\n",
    )])
    .expect("kotlinc builds the upper-bound dependency");
    let source = "import dependency.Box\nval bad: Box<String>? = null\n";
    let result =
        common::compiler_diagnostics(&[("Use.kt", source)], &[dependency, common::stdlib_jar()]);
    let krusty_output = format!("{}{}", result.krusty_stderr, result.krusty_stdout);
    let expected = [common::CompilerError {
        file: "Use.kt".to_string(),
        line: 2,
        column: 14,
        message: "type argument is not within its bounds: type parameter 'T (of class Box<T : Number>)' must be subtype of 'Number', but actual: 'String'.".to_string(),
    }];
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the invalid dependency use"
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the invalid dependency use"
    );
    assert_eq!(common::compiler_errors(&krusty_output), expected);
}
