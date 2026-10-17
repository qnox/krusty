//! kotlinc's class variance checker (`FirClassVarianceChecker`): a classifier type parameter
//! declared `in` or `out` occurring in a position its variance forbids is
//! `TYPE_VARIANCE_CONFLICT_ERROR`, or `TYPE_VARIANCE_CONFLICT_IN_EXPANDED_TYPE` for a direct
//! argument of a type written through a typealias. Definitely-non-null and flexible types are
//! walked only with `ReportTypeVarianceConflictsInDnnAndFlexible`.

use super::common;

const DNN_AND_FLEXIBLE: &str = "-XXLanguage:+ReportTypeVarianceConflictsInDnnAndFlexible";

/// Compile `source` with both compilers, passing `args` to each, and require the same complete
/// errors and no warnings, which must also be `errors`.
fn assert_matches_kotlinc(source: &str, args: &[&str], errors: &[String]) {
    let args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = common::compiler_diagnostics_with_shared_args(
        &[("Variance.kt", source)],
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
    assert!(kotlinc_warnings.is_empty());
    let rendered = kotlinc_errors
        .iter()
        .map(|error| format!("{}:{}: {}", error.line, error.column, error.message))
        .collect::<Vec<_>>();
    assert_eq!(rendered, errors);
    assert_eq!(result.krusty_code == 0, result.reference_code == 0);
}

fn conflict(position: &str, parameter: &str, declared: &str, occurs: &str, ty: &str) -> String {
    format!(
        "{position}: type parameter '{parameter}' is declared as '{declared}' but occurs in \
         '{occurs}' position in type '{ty}'."
    )
}

fn expanded(position: &str, parameter: &str, declared: &str, occurs: &str, ty: &str) -> String {
    format!(
        "{position}: type parameter '{parameter}' is declared as '{declared}' but occurs in \
         '{occurs}' position in abbreviated type '{ty}'."
    )
}

const MEMBERS: &str = "\
package p
typealias L<X> = MutableList<X>
interface Box<T>
class A<out T, in U, V>(val a: T, var b: T, val u: U) {
    fun f1(t: T) {}
    fun f2(): U = TODO()
    fun f3(l: List<T>, m: MutableList<T>, x: L<T>) {}
    fun f4(f: (T) -> U): Box<in T> = TODO()
    fun <X : T> f5() {}
    fun T.f6() {}
    context(c: T) fun f7() {}
    private fun p1(t: T) {}
    val p2: (U) -> Unit get() = TODO()
    fun f8(x: T & Any) {}
    fun f9(x: Map<out T, Box<T>>, y: Array<in T>) {}
    fun f10(): Box<out U>? = null
    fun f11(x: @UnsafeVariance T) {}
}
interface S<out T> : Box<T>
";

/// The member positions kotlinc walks, with the definitely-non-null `T & Any` skipped while the
/// feature is off.
fn members_errors(dnn: bool) -> Vec<String> {
    let a = "(of class A<out T, in U, V>)";
    let mut errors = vec![
        conflict("4:42", "T", "out", "invariant", &format!("T {a}")),
        conflict("4:52", "U", "in", "out", &format!("U {a}")),
        conflict("5:15", "T", "out", "in", &format!("T {a}")),
        conflict("6:15", "U", "in", "out", &format!("U {a}")),
        conflict("7:20", "T", "out", "in", &format!("List<T {a}>")),
        conflict(
            "7:39",
            "T",
            "out",
            "invariant",
            &format!("MutableList<T {a}>"),
        ),
        expanded(
            "7:48",
            "T",
            "out",
            "invariant",
            &format!("MutableList<T {a}>"),
        ),
        conflict("8:33", "T", "out", "in", &format!("Box<in T {a}>")),
        conflict("9:14", "T", "out", "in", &format!("T {a}")),
        conflict("10:9", "T", "out", "in", &format!("T {a}")),
        conflict("11:16", "T", "out", "in", &format!("T {a}")),
    ];
    if dnn {
        errors.push(conflict("14:15", "T", "out", "in", &format!("T {a} & Any")));
    }
    let map = format!("Map<out T {a}, Box<T {a}>>");
    errors.extend([
        conflict("15:23", "T", "out", "in", &map),
        conflict("15:30", "T", "out", "invariant", &map),
        conflict("16:24", "U", "in", "out", &format!("Box<out U {a}>?")),
        conflict(
            "19:26",
            "T",
            "out",
            "invariant",
            "Box<T (of interface S<out T>)>",
        ),
    ]);
    errors
}

#[test]
fn member_positions_match_kotlinc() {
    assert_matches_kotlinc(MEMBERS, &[], &members_errors(false));
}

#[test]
fn definitely_non_null_types_are_walked_with_the_feature() {
    assert_matches_kotlinc(MEMBERS, &[DNN_AND_FLEXIBLE], &members_errors(true));
}

const FLEXIBLE: &str = "\
package f
class A<in U> {
    fun f() = java.util.Collections.singletonList<U>(null)
}
";

/// A Java result is flexible; its lower bound is the mutable collection, whose argument is
/// invariant. kotlinc walks it only with the feature.
#[test]
fn flexible_types_are_walked_only_with_the_feature() {
    assert_matches_kotlinc(FLEXIBLE, &[], &[]);
    assert_matches_kotlinc(
        FLEXIBLE,
        &[DNN_AND_FLEXIBLE],
        &[conflict(
            "3:5",
            "U",
            "in",
            "invariant",
            "(Mutable)List<U! (of class A<in U>)>!",
        )],
    );
}

const DECLARATIONS: &str = "\
package q
typealias L<X> = MutableList<X>
typealias M<X> = Map<String, X>
interface Inv<T>
abstract class A<in T, out O> {
    public open fun f1() = emptyList<T>()
    val p1 = emptyList<T>()
    internal var p2: O? = null
    protected abstract val p3: Inv<O>
    @Suppress(\"x\") fun f2() = emptyList<T>()
    fun f3(vararg x: O, y: Array<O>) {}
    fun f4(m: M<O>) {}
    fun <X> f5(m: MutableMap<O, X>) {}
    val O.e1: Int get() = 1
    val <X : O> X.e2: Int get() = 1
    private var ok: O? = null
    fun f6(s: suspend O.(Int) -> Unit) {}
    fun f7(): (T) -> T = TODO()
    fun f8(c: Comparable<T>, o: Comparable<O>) {}
    var p4: Inv<*> = TODO()
}
class B<out T : Comparable<T>, in U, R : U>
interface C<out T> : Inv<@UnsafeVariance T>, Comparable<T>
";

#[test]
fn declarations_match_kotlinc() {
    let a = "(of class A<in T, out O>)";
    assert_matches_kotlinc(
        DECLARATIONS,
        &[],
        &[
            conflict("6:5", "T", "in", "out", &format!("List<T {a}>")),
            conflict("7:5", "T", "in", "out", &format!("List<T {a}>")),
            conflict("8:22", "O", "out", "invariant", &format!("O? {a}")),
            conflict("9:36", "O", "out", "invariant", &format!("Inv<O {a}>")),
            conflict("10:5", "T", "in", "out", &format!("List<T {a}>")),
            conflict("11:22", "O", "out", "in", &format!("Array<out O {a}>")),
            conflict("11:34", "O", "out", "invariant", &format!("Array<O {a}>")),
            expanded("12:15", "O", "out", "in", &format!("Map<String, O {a}>")),
            conflict(
                "13:30",
                "O",
                "out",
                "invariant",
                &format!("MutableMap<O {a}, X (of fun <X> f5)>"),
            ),
            conflict("14:9", "O", "out", "in", &format!("O {a}")),
            conflict("15:14", "O", "out", "in", &format!("O {a}")),
            conflict("18:22", "T", "in", "out", &format!("(T {a}) -> T {a}")),
            conflict("19:26", "T", "in", "out", &format!("Comparable<T {a}>")),
            conflict(
                "22:28",
                "T",
                "out",
                "in",
                "Comparable<T (of class B<out T : Comparable<T>, in U, R : U>)>",
            ),
            conflict(
                "22:42",
                "U",
                "in",
                "out",
                "U (of class B<out T : Comparable<T>, in U, R : U>)",
            ),
            conflict(
                "23:57",
                "T",
                "out",
                "in",
                "Comparable<T (of interface C<out T>)>",
            ),
        ],
    );
}

/// A property's type parameters keep their declaration owner while its receiver is walked. The
/// containing class's conflicting parameter and the invariant sibling property parameter are both
/// rendered with their exact owners, even though body checking has already retired the latter.
#[test]
fn property_type_parameter_owner_survives_the_variance_walk() {
    let source = "class A<out O> { val <X> MutableMap<O, X>.p: Int get() = 0 }";
    assert_matches_kotlinc(
        source,
        &[],
        &[conflict(
            "1:37",
            "O",
            "out",
            "invariant",
            "MutableMap<O (of class A<out O>), X (of val <X> MutableMap<O, X>.p)>",
        )],
    );
}

/// An inferred public property is checked at the property's declaration site, using its finalized
/// semantic type. Constructor properties precede body properties in the stable member inventory.
#[test]
fn inferred_property_uses_its_stable_member_identity_and_declaration_span() {
    let source = "class A<in T>(val prefix: Int) {\n    val inferred = emptyList<T>()\n}";
    assert_matches_kotlinc(
        source,
        &[],
        &[conflict(
            "2:5",
            "T",
            "in",
            "out",
            "List<T (of class A<in T>)>",
        )],
    );
}

/// An accessor-only return annotation is the property's declared semantic type. Its written
/// occurrence owns the diagnostic anchor and any alias or `@UnsafeVariance` provenance.
#[test]
fn getter_return_type_is_checked_as_the_declared_property_type() {
    let source = "class A<in T> {\n    val p get(): List<T> = TODO()\n}";
    assert_matches_kotlinc(
        source,
        &[],
        &[conflict(
            "2:23",
            "T",
            "in",
            "out",
            "List<T (of class A<in T>)>",
        )],
    );

    let safe = "class Safe<in T> {\n    val p get(): @UnsafeVariance T = TODO()\n    fun broken(): Missing = TODO()\n}";
    assert_matches_kotlinc(
        safe,
        &[],
        &["3:19: unresolved reference 'Missing'.".to_string()],
    );
}

/// Variance diagnostics belong to the declaration whose signature contains the occurrence. Its
/// lexical suppression policy is reopened for this post-body class check.
#[test]
fn member_declaration_suppressions_cover_variance_conflicts() {
    let source = r#"class A<out T> {
    @Suppress("TYPE_VARIANCE_CONFLICT_ERROR") fun direct(value: T) {}
}
class B<in T> {
    @Suppress("TYPE_VARIANCE_CONFLICT_ERROR") val produced: T get() = TODO()
}
class C<out T>(@Suppress("TYPE_VARIANCE_CONFLICT_ERROR") var value: T)
fun broken(): Missing = TODO()
"#;
    assert_matches_kotlinc(
        source,
        &[],
        &["8:15: unresolved reference 'Missing'.".to_string()],
    );
}

/// An alias template's resolved `@UnsafeVariance` applies to each occurrence of its formal. An
/// unannotated template that uses one formal twice reports both semantic occurrences: the first at
/// the alias application and the second at the written argument, in kotlinc's order.
#[test]
fn alias_template_annotations_and_duplicate_formals_match_kotlinc() {
    let source = "\
package aliasvariance

typealias Safe<X> = MutableMap<@UnsafeVariance X, @UnsafeVariance X>
typealias Duplicated<X> = MutableMap<X, X>

class A<out T> {
    fun safe(value: Safe<T>) {}
    fun duplicated(value: Duplicated<T>) {}
}
";
    let ty = "MutableMap<T (of class A<out T>), T (of class A<out T>)>";
    assert_matches_kotlinc(
        source,
        &[],
        &[
            expanded("8:27", "T", "out", "invariant", ty),
            expanded("8:38", "T", "out", "invariant", ty),
        ],
    );
}

const NESTED: &str = "\
package s
interface Inv<T>
abstract class Base<T>
class A<out O : Number, in I> : Base<O>() {
    inner class E<out S> { fun f(o: O, s: S) {} }
    class N<O> { fun g(o: O) {} }
    fun <U> h() { class Loc<out X> { fun g(x: X, u: U, o: O) {} } }
    val f7 = object : Inv<O> { fun g(i: I) = i }
    fun f8(): Inv<String>.(O) -> Unit = TODO()
    fun f9(): O?.() -> Unit = TODO()
    fun f10(): context(String, O) Int.(Long) -> Unit = TODO()
    fun f11(): ((O) -> Unit)? = TODO()
    inner class G<out S> { inner class F<in R> { fun g(s: S, r: R): Inv<O> = TODO() } }
}
fun <T> local() { class Loc<out X> { fun g(x: X, t: T) {} } }
";

/// Inner, local and anonymous classifiers see the variance of the formals they capture; a
/// classifier with formals of its own names the captured ones as `Outer(T)`.
#[test]
fn nested_classifiers_match_kotlinc() {
    let a = "(of class A<out O : Number, in I>)";
    assert_matches_kotlinc(
        NESTED,
        &[],
        &[
            conflict("4:38", "O", "out", "invariant", &format!("Base<O {a}>")),
            conflict("5:37", "O", "out", "in", &format!("O {a}")),
            conflict(
                "5:43",
                "S",
                "out",
                "in",
                "S (of class E<out S, out Outer(O) : Number, in Outer(I)>)",
            ),
            conflict(
                "7:47",
                "X",
                "out",
                "in",
                "X (of class Loc<out X, Outer(U), out Outer(O) : Number, in Outer(I)>)",
            ),
            conflict("7:59", "O", "out", "in", &format!("O {a}")),
            conflict("8:5", "O", "out", "invariant", &format!("Inv<O {a}>")),
            conflict("8:27", "O", "out", "invariant", &format!("Inv<O {a}>")),
            conflict("8:32", "I", "in", "out", &format!("I {a}")),
            conflict(
                "9:28",
                "O",
                "out",
                "in",
                &format!("Inv<String>.(O {a}) -> Unit"),
            ),
            conflict("10:15", "O", "out", "in", &format!("(O? {a}).() -> Unit")),
            conflict(
                "11:32",
                "O",
                "out",
                "in",
                &format!("context(String, O {a}) Int.(Long) -> Unit"),
            ),
            conflict("12:18", "O", "out", "in", &format!("((O {a}) -> Unit)?")),
            conflict(
                "13:59",
                "S",
                "out",
                "in",
                "S (of class G<out S, out Outer(O) : Number, in Outer(I)>)",
            ),
            conflict("13:73", "O", "out", "invariant", &format!("Inv<O {a}>")),
            conflict(
                "15:47",
                "X",
                "out",
                "in",
                "X (of class Loc<out X, Outer(T)>)",
            ),
        ],
    );
}
