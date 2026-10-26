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
fn explicit_inner_constructor_bounds_use_the_selected_enclosing_arguments() {
    let source = r#"class Outer<O : Number> {
    inner class Inner<T : O>(val value: T)
}
fun valid(outer: Outer<Int>): Outer<Int>.Inner<Int> = outer.Inner<Int>(1)
fun box(): String = if (valid(Outer<Int>()).value == 1) "OK" else "wrong value"
"#;
    common::expect_box_same_as_kotlinc(source, "inner_constructor_captured_bound");
}

#[test]
fn a_local_type_variable_is_an_argument_for_any_of_its_bounds() {
    let src = r#"fun takeCs(c: CharSequence) = c.length

fun outer(): Int {
    fun <T> inner(x: T): Int where T : Comparable<T>, T : CharSequence = takeCs(x)
    return inner("OK")
}

fun box(): String = if (outer() == 2) "OK" else "local bounds"
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_local_function");
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

#[test]
fn a_definitely_non_null_type_variable_meets_every_non_null_bound() {
    let src = r#"fun takeCs(c: CharSequence) = c.length
fun takeComparable(c: Comparable<String>) = c.compareTo("b")
fun <T> length(a: T & Any): Int where T : Comparable<T>?, T : CharSequence? = takeCs(a)
fun <T : Comparable<String>?> compared(a: T & Any): Int where T : CharSequence? = takeComparable(a)

fun box(): String {
    val n = length<String>("abc")
    if (n != 3) return "length $n"
    val c = compared<String?>("b")
    if (c != 0) return "compared $c"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_definitely_non_null");
}

#[test]
fn a_chained_variable_meets_a_class_bound_written_after_an_interface() {
    let src = r#"fun takeI(i: Int) = i + 1
fun <T : X, X : Comparable<Int>> next(a: T) where X : Int = takeI(a)
fun <T : X, X> nextWhere(a: T) where X : Comparable<Int>, X : Int = takeI(a)

fun box(): String {
    val results = listOf(next(1), nextWhere(2))
    return if (results == listOf(2, 3)) "OK" else "$results"
}
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_chained_class_bound");
}

#[test]
fn a_reordered_bound_is_recarried_inside_another_parameters_class_bound() {
    let src = r#"interface Marker
open class Box<T>(val value: T)
class IntBox : Box<Int>(41), Marker

fun <X, T> take(box: T): X
    where X : Comparable<Int>, X : Int, T : Marker, T : Box<X> = box.value

fun box(): String = if (take<Int, IntBox>(IntBox()) == 41) "OK" else "nested bound"
"#;
    common::expect_box_same_as_kotlinc(src, "where_bound_nested_recarry");
}

/// A property's `where` clause precedes its initializer or delegate, as the grammar orders
/// `typeConstraints` before `= expression` / `by expression`.
#[test]
fn an_extension_property_where_clause_precedes_its_delegate() {
    let source = "import kotlin.properties.ReadOnlyProperty\n\
         \n\
         interface Named { val name: String }\n\
         interface Sized { val size: Int }\n\
         class Both : Named, Sized { override val name = \"O\"; override val size = 1 }\n\
         \n\
         fun <T> label(): ReadOnlyProperty<T, String> where T : Named, T : Sized =\n\
         \x20   ReadOnlyProperty { thisRef, _ -> thisRef.name + \"K\".repeat(thisRef.size) }\n\
         \n\
         val <T> T.label: String where T : Named, T : Sized by label()\n\
         \n\
         fun box(): String = Both().label\n";
    common::expect_box_ok_with_stdlib(source, "WhereDelegate");
    let classes = common::classes_against_kotlinc_module(&[("WhereDelegate.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
