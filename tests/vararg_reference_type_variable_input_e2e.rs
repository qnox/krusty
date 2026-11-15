//! A callable reference to a generic `vararg` function, passed where the enclosing call declares
//! the function's input as one of its own type variables (`f: (T) -> Int`). kotlinc releases with
//! `Quirk::VarargReferenceTakesTypeVariableInputAsElement` (KT-90169) bind that input as one
//! collected element; later ones bind the whole array, as they already do for a concrete or
//! array-shaped input. The fixture is not a shared box regression because the two behaviors
//! return different values, so no single `box()` result is `OK` on every release.

use krusty::kotlin_version::{Quirk, Quirks};

use super::common;

const SOURCE: &str = r#"
fun <T> applyIt(t: T, f: (T) -> Int): Int = f(t)
fun <T, R> applyR(t: T, f: (T) -> R): R = f(t)
fun <T> applyNullable(t: T, f: (T?) -> Int): Int = f(t)
fun <T> explicit(f: (T) -> Int): (T) -> Int = f
fun <T> applyArray(t: Array<T>, f: (Array<T>) -> Int): Int = f(t)
fun <E> countG(vararg xs: E): Int = xs.size
fun countS(vararg xs: String): Int = xs.size
fun countI(vararg xs: Int): Int = xs.size
fun <E> firstG(vararg xs: E): E = xs[0]
fun <E> listG(vararg xs: E): List<E> = xs.toList()

class Box {
    fun <E> count(vararg xs: E): Int = xs.size
}
fun <E> Box.countX(vararg xs: E): Int = xs.size

class Holder<T>(val t: T) {
    fun run(f: (T) -> Int): Int = f(t)
    fun <U> runOwn(u: U, f: (U) -> Int): Int = f(u)
}

fun box(): String {
    val arr = arrayOf("a", "b")
    fun <E> localCount(vararg xs: E): Int = xs.size
    val typed: (Array<String>) -> Int = ::countG
    return listOf(
        "top=" + applyIt(arr, ::countG),
        "explicit=" + explicit<Array<String>>(::countG)(arr),
        "let=" + arr.let(::countG),
        "nullable=" + applyNullable(arr, ::countG),
        "result=" + (applyR(arr, ::firstG) is Array<*>),
        "list=" + applyR(arr, ::listG).size,
        "member=" + applyIt(arr, Box()::count),
        "extension=" + applyIt(arr, Box()::countX),
        "local=" + applyIt(arr, ::localCount),
        "ownFormal=" + Holder(1).runOwn(arr, ::countG),
        "receiverFormal=" + Holder(arr).run(::countG),
        "arrayInput=" + applyArray(arr, ::countG),
        "concreteVararg=" + applyIt(arr, ::countS),
        "primitive=" + applyIt(intArrayOf(1, 2, 3), ::countI),
        "declared=" + typed(arr),
    ).joinToString(",")
}
"#;

/// What every case returns: `element` where an input from a call type variable binds one element.
fn expected(element: bool) -> String {
    let (count, first) = if element { (1, true) } else { (2, false) };
    [
        format!("top={count}"),
        format!("explicit={count}"),
        format!("let={count}"),
        format!("nullable={count}"),
        format!("result={first}"),
        format!("list={count}"),
        format!("member={count}"),
        format!("extension={count}"),
        format!("local={count}"),
        format!("ownFormal={count}"),
        "receiverFormal=2".to_string(),
        "arrayInput=2".to_string(),
        "concreteVararg=2".to_string(),
        "primitive=3".to_string(),
        "declared=2".to_string(),
    ]
    .join(",")
}

#[test]
fn a_type_variable_input_binds_the_vararg_as_the_reference_release_does() {
    let element = Quirks::target().has(Quirk::VarargReferenceTakesTypeVariableInputAsElement);
    let reference = common::kotlinc_box_result(SOURCE);
    assert_eq!(reference, expected(element), "kotlinc");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let krusty = common::compile_and_run_box(
        SOURCE,
        "Main",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    assert_eq!(krusty.as_deref(), Some(reference.as_str()), "krusty");
}
