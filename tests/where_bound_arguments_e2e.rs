//! A type variable is a subtype of every one of its upper bounds, including those a `where` clause
//! adds after the first: it is an argument for a parameter of any of them, through another type
//! variable's bounds too, and selects an operator such as `contains` the same way.

use super::common;

#[test]
fn a_type_variable_is_an_argument_for_any_of_its_bounds() {
    let src = r#"fun takeCs(c: CharSequence) = c.length
fun takeU(u: UInt) = u.toInt()
fun <T> second(a: T) where T : Comparable<T>, T : CharSequence = takeCs(a)
fun <T : X, X> chained(a: T) where X : Comparable<X>, X : CharSequence = takeCs(a)
fun <T : X, X : Comparable<UInt>> unsigned(a: T) where X : UInt = takeU(a)
fun <T : X, X : Comparable<UInt>> contained(a: T): Boolean where X : UInt = (1U..10U).contains(a)
fun <T : X, X : Comparable<Int>> member(a: T): Boolean where X : Int = a in 1..10

fun box(): String {
    val results = listOf(second("ab"), chained("abc"), unsigned(4U))
    if (results != listOf(2, 3, 4)) return "$results"
    if (!contained(3U) || contained(11U)) return "contains"
    if (!member(3) || member(0)) return "in"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_arguments");
}

#[test]
fn source_constructor_primary_selection_sees_a_type_variables_later_bound() {
    let src = r#"class Cell<T>(val value: T, marker: CharSequence) {
    constructor(value: T, marker: Int) : this(value, marker.toString())
}

fun <X> make(marker: X): Cell<String>
    where X : Comparable<X>, X : CharSequence = Cell("OK", marker)

fun box(): String = make("ignored").value
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_source_constructor");
}

#[test]
fn later_bounds_survive_variance_and_a_nothing_branch() {
    let src = r#"interface Source<out T> { fun get(): T }
class TextSource(private val text: String) : Source<String> { override fun get() = text }
interface Sink<in T> { fun put(value: T) }
class TextSink : Sink<CharSequence> {
    var value = ""
    override fun put(value: CharSequence) { this.value = value.toString() }
}

fun <T> widen(source: Source<T>): Source<CharSequence>
    where T : Comparable<T>, T : CharSequence = source
fun <T> narrow(sink: Sink<CharSequence>): Sink<T>
    where T : Comparable<T>, T : CharSequence = sink
fun <T> keepOrThrow(value: T, fail: Boolean): T
    where T : Comparable<T>, T : CharSequence = if (fail) error("stop") else value

fun box(): String {
    val source = widen(TextSource("O"))
    val sink = TextSink()
    narrow<String>(sink).put(keepOrThrow("K", false))
    return source.get().toString() + sink.value
}
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_variance_and_nothing");
}
