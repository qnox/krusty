//! An anonymous object that a classpath `inline` function's body creates is regenerated for each
//! call site as a class of the caller's (`<caller>$<function>$$inlined$<callee>$<n>`), the way
//! kotlinc's `AnonymousObjectTransformer` copies it. The inline functions are this repository's own,
//! compiled by the reference compiler, so the copies are made from kotlinc's classes.

use super::common;

const LIB: &str = r#"
package lib

interface Greeter {
    fun greet(): String
}

interface Box<T> {
    fun open(): T
}

open class Item(val name: String)

class Wrap<out T>(val value: T)

inline fun twice(block: () -> Unit) {
    block()
    block()
}

inline fun greeter(prefix: String): Greeter = object : Greeter {
    override fun greet(): String = prefix + "!"
}

inline fun counter(start: Long, step: Int): Greeter = object : Greeter {
    val label = "count"

    override fun greet(): String {
        var text = label
        twice { text = text + (start + step) }
        return text
    }
}

inline fun <T> boxed(value: T): Box<T> = object : Box<T> {
    override fun open(): T = value
}

inline fun pair(first: String, second: String): String =
    object : Greeter { override fun greet(): String = first }.greet() +
        object : Greeter { override fun greet(): String = second }.greet()
"#;

const MAIN: &str = r#"
import lib.*

fun plain(): String = greeter("a").greet()

fun both(): String = greeter("b").greet() + greeter("c").greet()

fun counted(): String = counter(40L, 2).greet()

fun opened(): String = boxed("box").open()

fun wrapped(): String = boxed(Wrap(Item("w"))).open().value.name

fun <U> forwarded(value: U): U = boxed(value).open()

fun forwardedText(): String = forwarded("f")

fun numbered(): Int = boxed(7).open()

fun paired(): String = pair("x", "y")

fun box(): String {
    if (plain() != "a!") return "FAIL plain: " + plain()
    if (both() != "b!c!") return "FAIL both: " + both()
    if (counted() != "count4242") return "FAIL counted: " + counted()
    if (opened() != "box") return "FAIL opened: " + opened()
    if (paired() != "xy") return "FAIL paired: " + paired()
    if (wrapped() != "w") return "FAIL wrapped: " + wrapped()
    if (forwardedText() != "f") return "FAIL forwarded: " + forwardedText()
    if (numbered() != 7) return "FAIL numbered: " + numbered()
    return "OK"
}
"#;

#[test]
fn regenerated_objects_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("inline_object_regeneration", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The caller and every copy are kotlinc's byte for byte: the copies' names, members, constant
/// pools, `@Metadata`, source maps and `EnclosingMethod`, and the calls that construct them.
#[test]
fn regenerated_objects_are_the_reference_compilers_classes() {
    let classes = common::classes_against_kotlinc_lib("Regeneration", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "RegenerationKt",
            "RegenerationKt$both$$inlined$greeter$1",
            "RegenerationKt$both$$inlined$greeter$2",
            "RegenerationKt$counted$$inlined$counter$1",
            "RegenerationKt$forwarded$$inlined$boxed$1",
            "RegenerationKt$numbered$$inlined$boxed$1",
            "RegenerationKt$opened$$inlined$boxed$1",
            "RegenerationKt$paired$$inlined$pair$1",
            "RegenerationKt$paired$$inlined$pair$2",
            "RegenerationKt$plain$$inlined$greeter$1",
            "RegenerationKt$wrapped$$inlined$boxed$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
