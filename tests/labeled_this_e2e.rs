//! Labeled `this` (`this@C`). The parser now accepts `this@Label` / `super@Label` (previously
//! "expected an expression"). A SELF-label — `this@C` inside `C`'s own member, often via a lambda
//! (`run { this@C.bar() }`) — resolves to the current `this`. Outer-class / receiver-lambda / accessor
//! labels need a receiver-label stack krusty does not track yet (those files skip, never miscompile).

use super::common;

#[test]
fn self_labeled_this_in_lambda() {
    // `this@C` inside a lambda in C's own method resolves to C's receiver.
    const SRC: &str = "class C(val v: String) {\n\
    fun foo(): String = run { this@C.bar() }\n\
    fun bar(): String = v\n\
}\n\
fun box(): String = C(\"OK\").foo()\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}

/// `this@Outer` from an `inner class` — the immediate enclosing class, reached via the captured
/// `this$0`. Both the bare member (`v`) and the qualified `this@B.v` must read the outer instance.
#[test]
fn inner_class_outer_labeled_this() {
    const SRC: &str = "class B {\n\
    val v = \"OK\"\n\
    inner class C {\n\
        fun g(): String = this@B.v\n\
    }\n\
}\n\
fun box(): String = B().C().g()\n";
    common::expect_box_ok_with_stdlib(SRC, "Main");
}
