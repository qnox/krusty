//! A vararg's element type competes with a fixed parameter in one specificity comparison. A
//! strictly narrower element wins; equal types keep the declaration that has no vararg. The same
//! policy applies to member and top-level overloads, contextual calls, and integer adaptation.

use super::common;

#[test]
fn member_vararg_element_specificity_matches_kotlinc() {
    let source = r#"
open class Root
class Leaf : Root()
class Crate<out T> : Root()
fun <T> crate(): Crate<T> = Crate<T>()

class All<T> {
    fun equalRoot(vararg values: Root) = "vararg"
    fun equalRoot(value: Root) = "fixed"

    fun equalT(vararg values: T) = "vararg"
    fun equalT(value: T) = "fixed"

    fun narrower(vararg values: Crate<Leaf>) = "vararg"
    fun narrower(value: Root) = "fixed"

    fun covariant(vararg values: Crate<Leaf>) = "vararg"
    fun covariant(value: Crate<Root>) = "fixed"

    fun two(vararg values: Crate<Leaf>) = "vararg"
    fun two(first: Root, second: Root) = "fixed"

    fun leading(first: Crate<Leaf>, vararg rest: Crate<Leaf>) = "vararg"
    fun leading(first: Root, second: Root) = "fixed"

    fun defaults(value: Leaf = Leaf()) = "fixed"
    fun defaults(vararg value: Leaf) = "vararg"
}

fun box(): String {
    val all: All<Root> = All()
    val typed: Crate<Leaf> = crate()
    val actual =
        all.equalRoot(typed) + "/" +
        all.equalT(typed) + "/" +
        all.narrower(typed) + "/" +
        all.narrower(crate()) + "/" +
        all.covariant(typed) + "/" +
        all.covariant(crate()) + "/" +
        all.two(typed, typed) + "/" +
        all.two(crate(), crate()) + "/" +
        all.leading(typed, typed) + "/" +
        all.defaults() + "/" +
        all.defaults(Leaf())
    return if (actual == "fixed/fixed/vararg/vararg/vararg/vararg/vararg/vararg/vararg/fixed/fixed") {
        "OK"
    } else {
        actual
    }
}
"#;

    common::expect_box_same_as_kotlinc(source, "MemberVarargElementSpecificity");
}

#[test]
fn top_level_contextual_and_integer_vararg_ties_match_kotlinc() {
    let source = r#"
open class Root
class Leaf : Root()
class Crate<out T> : Root()
fun <T> crate(): Crate<T> = Crate<T>()

fun equal(vararg values: Crate<Leaf>) = "vararg"
fun equal(value: Crate<Leaf>) = "fixed"

fun narrower(vararg values: Crate<Leaf>) = "vararg"
fun narrower(value: Root) = "fixed"

fun integer(vararg value: Byte) = "vararg"
fun integer(value: Byte) = "fixed"

fun box(): String {
    val typed: Crate<Leaf> = crate()
    val actual =
        equal(typed) + "/" +
        equal(crate()) + "/" +
        narrower(typed) + "/" +
        narrower(crate()) + "/" +
        integer(1)
    return if (actual == "fixed/fixed/vararg/vararg/fixed") "OK" else actual
}
"#;

    common::expect_box_same_as_kotlinc(source, "TopLevelVarargElementSpecificity");
}

#[test]
fn classpath_contextual_vararg_tie_matches_kotlinc() {
    const LIB: &str = r#"
package dep

class Leaf
class Crate<out T>

fun <T> crate(): Crate<T> = Crate<T>()

fun equal(vararg values: Crate<Leaf>) = "vararg"
fun equal(value: Crate<Leaf>) = "fixed"
"#;
    const MAIN: &str = r#"
import dep.*

fun box(): String {
    val typed: Crate<Leaf> = crate()
    val actual = equal(typed) + "/" + equal(crate())
    return if (actual == "fixed/fixed") "OK" else actual
}
"#;

    let Some(reference_library) = common::kotlinc_library(LIB) else {
        return;
    };
    let reference = common::kotlinc_box_result_with_classpath(MAIN, &[reference_library]);
    assert_eq!(reference, "OK", "kotlinc box() = {reference:?}");
    let Some(actual) = common::expect_box_run_against("classpath_vararg_tie", LIB, MAIN) else {
        return;
    };
    assert_eq!(actual, reference, "krusty and kotlinc box results differ");
}
