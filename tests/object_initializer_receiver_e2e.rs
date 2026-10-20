//! An object's static initializer reads `this` as kotlinc does: each read loads the published
//! `INSTANCE` again, with no local holding it, and a property's own initializer store loads no
//! receiver at all.

use super::common;

fn byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => eprintln!("skip ({name}: reference toolchain unavailable)"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// An initializer calling an inherited member passes `INSTANCE` as the receiver of each call.
#[test]
fn an_inherited_call_in_an_initializer_loads_the_instance_for_each_call() {
    byte_identical(
        "object_initializer_inherited_call",
        r#"
abstract class Base {
    fun state(name: String, size: Int = 0): String = name + size
}
object Holder : Base() {
    val id: String = state("id", 1)
    val other: String = state("other", 2)
}
"#,
        "Holder",
    );
}

/// Reads and writes of the object's own property keep the receiver's load for its effects, while
/// the property initializers' stores have no receiver.
#[test]
fn own_property_accesses_load_the_instance_and_initializer_stores_do_not() {
    byte_identical(
        "object_initializer_own_property",
        r#"
object Counter {
    var x: Int = 1
    val y: Int = x + 1
    val z: Int = this.x + 2
    init { x = 5; this.x = 6 }
}
"#,
        "Counter",
    );
}

/// An `init` block that passes `this` on loads `INSTANCE` where it reads it.
#[test]
fn an_init_block_passing_this_loads_the_instance() {
    byte_identical(
        "object_initializer_passes_this",
        r#"
object Registry {
    fun f(s: String): String = s
    val a: String = f("a")
    init {
        println(this)
        println(f("i"))
    }
    val b: String = f("b")
}
"#,
        "Registry",
    );
}

/// An inlined extension lambda in an initializer binds its own receiver to value 0. That binding
/// keeps its local; only the object's unbound `this` loads `INSTANCE`.
#[test]
fn an_inlined_receiver_in_an_initializer_keeps_its_local() {
    common::expect_box_ok_with_stdlib(
        r#"
inline fun <T> T.with(f: T.() -> Unit): T {
    this.f()
    return this
}
object Buffered {
    val string = "OK"
    val buffer = StringBuilder().with {
        append(string)
    }
}
fun box(): String = Buffered.buffer.toString()
"#,
        "object_initializer_inlined_receiver",
    );
}

/// An inlined lambda's parameter in an initializer is a local of the inlined body, not the
/// object's receiver, even when the value it holds is a nested anonymous object.
#[test]
fn an_inlined_lambda_parameter_in_an_initializer_keeps_its_local() {
    common::expect_box_ok_with_stdlib(
        r#"
inline fun <T> T.also(f: (T) -> Unit): T {
    f(this)
    return this
}
object Holder {
    var result = ""
    private val made = object {
        fun inner() = object {
            fun mark() {
                result += "K"
            }
        }.also { result += "O" }
    }
    val kept = made.inner().also { it.mark() }
}
fun box(): String {
    Holder.kept
    return Holder.result
}
"#,
        "object_initializer_inlined_parameter",
    );
}
