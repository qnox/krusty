//! A call through an interface another file of the module declares.
//!
//! An interface member has one module-wide number, decided from the declaration headers alone, and
//! every class's interface table holds its implementation of that member at that number. The
//! calling file needs nothing but the number, so the interface, its implementors and the caller can
//! sit in separate files. Each program is first run on the JVM, then on Native.

use crate::common::{
    expect_box_ok_files_with_stdlib, expect_native_sources, expect_native_sources_decline,
};

/// A portable multi-file program: the JVM answer is established first, then Native must agree.
fn expect_portable_sources(sources: &[(&str, &str)]) {
    let stem = sources.first().map(|(stem, _)| *stem).unwrap_or("module");
    expect_box_ok_files_with_stdlib(sources, stem);
    expect_native_sources(sources, "OK");
}

#[test]
fn an_interface_call_reaches_implementors_of_other_files() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun total(shapes: List<Shape>): Int {
                    var sum = 0
                    for (shape in shapes) sum += shape.area(2)
                    return sum
                }
                fun box(): String {
                    val shapes = listOf(Square(3), Rectangle(2, 5))
                    return if (total(shapes) == 38 && shapes[1].name() == "rectangle") "OK"
                    else "FAIL"
                }
            "#,
        ),
        (
            "shape",
            r#"
                package demo
                interface Shape {
                    fun area(scale: Int): Int
                    fun name(): String
                }
            "#,
        ),
        (
            "square",
            r#"
                package demo
                class Square(private val side: Int) : Shape {
                    override fun area(scale: Int): Int = side * side * scale
                    override fun name(): String = "square"
                }
            "#,
        ),
        (
            "rectangle",
            r#"
                package demo
                class Rectangle(private val width: Int, private val height: Int) : Shape {
                    override fun name(): String = "rectangle"
                    override fun area(scale: Int): Int = width * height * scale
                }
            "#,
        ),
    ]);
}

#[test]
fn an_inherited_implementation_answers_for_a_subclass() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun describe(named: Named): String = named.name()
                fun box(): String =
                    if (describe(Leaf()) == "base" && describe(Override()) == "override") "OK"
                    else "FAIL"
            "#,
        ),
        (
            "named",
            r#"
                package demo
                interface Named {
                    fun name(): String
                }
            "#,
        ),
        (
            "base",
            r#"
                package demo
                open class Base {
                    fun name(): String = "base"
                }
                class Leaf : Base(), Named
                open class Open : Named {
                    override fun name(): String = "open"
                }
                class Override : Open() {
                    override fun name(): String = "override"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_member_of_an_extended_interface_keeps_its_number() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun first(source: Source): String = source.read()
                fun second(sink: Pipe): String = sink.write("x") + sink.read()
                fun box(): String {
                    val pipe = Buffer()
                    return if (first(pipe) == "" && second(pipe) == "1x") "OK" else "FAIL"
                }
            "#,
        ),
        (
            "interfaces",
            r#"
                package demo
                interface Source {
                    fun read(): String
                }
                interface Sink {
                    fun write(text: String): String
                }
                interface Pipe : Source, Sink
            "#,
        ),
        (
            "buffer",
            r#"
                package demo
                class Buffer : Pipe {
                    private var text = ""
                    override fun write(text: String): String {
                        this.text += text
                        return this.text.length.toString()
                    }
                    override fun read(): String = text
                }
            "#,
        ),
    ]);
}

#[test]
fn a_property_reads_and_writes_through_an_interface() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun bump(counter: Counter): Int {
                    counter.count = counter.count + 1
                    return counter.count + counter.step
                }
                fun box(): String = if (bump(Simple()) == 11) "OK" else "FAIL"
            "#,
        ),
        (
            "counter",
            r#"
                package demo
                interface Counter {
                    var count: Int
                    val step: Int
                }
            "#,
        ),
        (
            "simple",
            r#"
                package demo
                class Simple : Counter {
                    override var count: Int = 0
                    override val step: Int get() = 10
                }
            "#,
        ),
    ]);
}

#[test]
fn a_default_in_the_implementors_file_answers_through_the_interface() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun greet(greeter: Greeter): String = greeter.greeting() + greeter.target()
                fun box(): String =
                    if (greet(Plain()) == "hello world" && greet(Custom()) == "hi world") "OK"
                    else "FAIL"
            "#,
        ),
        (
            "greeter",
            r#"
                package demo
                interface Greeter {
                    fun greeting(): String = "hello "
                    fun target(): String
                }
                class Plain : Greeter {
                    override fun target(): String = "world"
                }
                class Custom : Greeter {
                    override fun greeting(): String = "hi "
                    override fun target(): String = "world"
                }
            "#,
        ),
    ]);
}

#[test]
fn a_default_declared_in_another_file_answers_for_an_implementor() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun box(): String {
                    val greeter: Greeter = Plain()
                    return if (greeter.greeting() == "hello" && greeter.name() == "plain") "OK"
                    else "FAIL"
                }
            "#,
        ),
        (
            "greeter",
            r#"
                package demo
                interface Greeter {
                    fun name(): String
                    fun greeting(): String = "hello"
                }
            "#,
        ),
        (
            "plain",
            r#"
                package demo
                class Plain : Greeter {
                    override fun name(): String = "plain"
                }
            "#,
        ),
    ]);
}

/// An interface that redeclares `toString` leaves it in `kotlin.Any`'s fixed slot, so a call
/// through the interface from another file reads that slot of the receiver's class table.
#[test]
fn a_redeclared_any_member_is_called_through_an_interface_of_another_file() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun render(label: Label): String = label.toString() + label.hashCode()
                fun box(): String {
                    val result = render(Tag("t"))
                    return if (result == "tag t7") "OK" else result
                }
            "#,
        ),
        (
            "label",
            r#"
                package demo
                interface Label {
                    override fun toString(): String
                    override fun hashCode(): Int
                }
            "#,
        ),
        (
            "tag",
            r#"
                package demo
                class Tag(private val text: String) : Label {
                    override fun toString(): String = "tag " + text
                    override fun hashCode(): Int = 7
                }
            "#,
        ),
    ]);
}

#[test]
fn a_default_reached_through_a_bridge_of_another_file_declines() {
    let sources: &[(&str, &str)] = &[
        (
            "box",
            r#"
                    package demo
                    class Seven : IntSource
                    fun box(): String {
                        val source: Source<Int> = Seven()
                        return if (source.next() == 7) "OK" else "FAIL"
                    }
                "#,
        ),
        (
            "source",
            r#"
                    package demo
                    interface Source<T> {
                        fun next(): T
                    }
                "#,
        ),
        (
            "ints",
            r#"
                    package demo
                    interface IntSource : Source<Int> {
                        override fun next(): Int = 7
                    }
                "#,
        ),
    ];
    expect_native_sources_decline(
        sources,
        "krusty: the native backend does not support an interface default reached through a \
         bridge, declared in another file (`demo.IntSource`) yet",
    );
}

#[test]
fn a_generic_member_reaches_an_implementation_through_its_bridge() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun <T> take(source: Source<T>): T = source.next()
                fun box(): String {
                    val counted: Source<Int> = Counting(41)
                    return if (take(counted) == 42 && take(Fixed("x")) == "x") "OK" else "FAIL"
                }
            "#,
        ),
        (
            "source",
            r#"
                package demo
                interface Source<T> {
                    fun next(): T
                }
                class Counting(private val start: Int) : Source<Int> {
                    override fun next(): Int = start + 1
                }
                class Fixed(private val text: String) : Source<String> {
                    override fun next(): String = text
                }
            "#,
        ),
    ]);
}

#[test]
fn a_lambda_converted_to_a_functional_interface_answers_another_file() {
    expect_portable_sources(&[
        (
            "box",
            r#"
                package demo
                fun box(): String = if (apply(twice(), 4) == 8) "OK" else "FAIL"
            "#,
        ),
        (
            "transform",
            r#"
                package demo
                fun interface Transform {
                    fun run(value: Int): Int
                    fun describe(): String = "transform"
                }
                fun twice(): Transform = Transform { it * 2 }
                fun apply(transform: Transform, value: Int): Int =
                    if (transform.describe() == "transform") transform.run(value) else -1
            "#,
        ),
    ]);
}
