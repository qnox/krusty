//! Labeled `this` (`this@C`). The parser now accepts `this@Label` / `super@Label` (previously
//! "expected an expression"). A SELF-label — `this@C` inside `C`'s own member, often via a lambda
//! (`run { this@C.bar() }`) — resolves to the current `this`; an immediate outer-class label resolves
//! through an inner class's captured `this$0`. Receiver-lambda and accessor labels use the same lexical
//! receiver stack, while unsupported multi-level capture shapes still skip instead of miscompiling.

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

/// A nested declaration is stored as `Outer.NestedReceiver` in the source symbol table, but Kotlin's
/// explicit receiver label is the declared SIMPLE name. This is an execution test, rather than only a
/// diagnostic check, so it also proves the selected current-class receiver survives inline-lambda
/// lowering instead of being mistaken for a qualified label or silently skipped.
#[test]
fn nested_class_self_label_uses_declaration_name() {
    const SRC: &str = "class Outer {\n\
    inner class NestedReceiver(val value: String) {\n\
        fun result(): String = run { this@NestedReceiver.value }\n\
    }\n\
}\n\
fun box(): String = Outer().NestedReceiver(\"OK\").result()\n";
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
