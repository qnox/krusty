//! A builder lambda that adds values of different subclasses fixes the builder's type variable to
//! their nearest common supertype, as kotlinc's PCLA does. The type variable's lower bounds are
//! joined against the class hierarchy, not erased to `Any`.

use super::common;

const SOURCE: &str = r#"sealed class Shape {
    class Dot(val n: Int) : Shape()
    class Line(val s: String) : Shape()
    class Group(val entries: Map<String, Shape>) : Shape()
    class Many(val shapes: List<Shape>) : Shape()
}

class Collector<E> {
    val items = ArrayList<E>()
    fun add(item: E) { items.add(item) }
}

fun <E> collect(block: Collector<E>.() -> Unit): List<E> = Collector<E>().apply(block).items

class Drawing(val shapes: List<Shape>)

fun grouped(flag: Boolean, extra: Shape.Group): Shape.Group =
    Shape.Group(buildMap { put("a", Shape.Dot(1)); put("b", Shape.Line("x")); if (flag) put("c", extra) })

fun optional(name: String?): Shape.Group =
    Shape.Group(buildMap { put("a", Shape.Dot(1)); name?.let { put("n", Shape.Line(it)) } })

fun many(): Shape.Many = Shape.Many(collect { add(Shape.Dot(2)); add(Shape.Line("y")) })

fun drawing(): Drawing = Drawing(collect { add(Shape.Dot(2)); add(Shape.Line("y")) })

fun box(): String {
    if (grouped(true, Shape.Group(emptyMap())).entries.size != 3) return "grouped"
    if (optional("q").entries.size != 2) return "optional"
    if (many().shapes.size != 2) return "many"
    if (drawing().shapes.size != 2) return "drawing"
    return "OK"
}
"#;

/// `buildMap`'s `V` and a repository-owned builder's `E` are fixed to `Shape` from sibling
/// subclass arguments: unconditionally, under an `if`, and inside a `?.let` lambda. The qualified
/// `Shape.Group(...)` / `Shape.Many(...)` constructor calls are the shapes that solved the builder
/// from its own lower bounds alone; the unqualified `Drawing(...)` call guards the path that already
/// took them from the parameter type.
#[test]
fn sibling_subclass_arguments_fix_the_builder_type_argument_to_their_supertype() {
    common::assert_accepted_like_kotlinc(SOURCE);
    common::expect_box_same_as_kotlinc(SOURCE, "BuilderSiblingLowerBoundsRun");
}
