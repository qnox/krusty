//! A result-only nested call is solved from the concrete parts of the enclosing parameter.
//!
//! The outer variables that are still open are not evidence, and the nested call is not completed
//! to its declared upper bound before those variables are bound.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_result_only_argument_is_solved_from_concrete_parameter_parts() {
    expect_box_same_as_kotlinc(
        r#"
import java.util.stream.Collectors
import java.util.stream.Stream

interface Box<T, A, R>
fun <T> starred(): Box<T, *, List<T>> = null!!
fun <T> indexed(): Box<T, Int, List<T>> = null!!
fun <T> captured(): Box<T, List<T>, *> = null!!
interface Src<S>
fun <S> src(): Src<S> = null!!
fun <S, R, A> Src<S>.gather(c: Box<in S, A, R>): R = null!!

fun starredResult(): List<String> {
    val paths = src<String>().gather(starred())
    return paths
}

fun indexedResult(): List<String> {
    val values: List<String> = src<String>().gather(indexed())
    return values
}

fun capturedResult(): String {
    val value = src<String>().gather(captured())
    return value.toString()
}

fun <E> take(x: List<E>): List<E> = x

fun box(): String {
    val list: MutableList<String> = Stream.of("a", "b").collect(Collectors.toList())
    if (list != listOf("a", "b")) return "list"
    val kept: List<String> = take(emptyList())
    if (kept.isNotEmpty()) return "empty"
    return "OK"
}
"#,
        "NestedConcreteArgument",
    );
}
