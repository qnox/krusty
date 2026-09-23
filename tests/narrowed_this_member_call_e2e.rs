//! A bare call resolved against a flow-narrowed implicit receiver: `if (this is B) foo()` inside a
//! member (or extension) body, where `foo` is a member of the subtype `B`, not the declared receiver
//! `A`. The checker resolves the call through `this_narrow` and records the narrowing; the lowerer
//! `checkcast`s `this` to `B` before dispatching. Same-file, runs on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn narrowed_this_resolves_subtype_member_call() {
    // `A.test()` narrows `this` (declared `A`) to `B` via `this is B`, then calls `B`'s own `foo()`.
    const SRC: &str = "\
open class A {\n\
    fun test(): Int = if (this is B) foo() else 0\n\
}\n\
class B : A() {\n\
    fun foo() = 42\n\
}\n\
fun box(): String {\n\
    if (B().test() != 42) return \"f1\"\n\
    if (A().test() != 0) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("narrowed this member call"), "OK");
}

#[test]
fn narrowed_this_call_passes_arguments() {
    // Regression companion: the narrowed dispatch must forward arguments and the result type.
    const SRC: &str = "\
open class A {\n\
    fun pick(): Int = if (this is B) plus(40, 2) else -1\n\
}\n\
class B : A() {\n\
    fun plus(x: Int, y: Int) = x + y\n\
}\n\
fun box(): String = if (B().pick() == 42) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("narrowed this call with args"), "OK");
}

/// A bare member READ against the narrowed `this`, used as the RECEIVER of a call.
///
/// The narrowing belongs to the implicit `this` inside `node`, not to the `Node` that `node`
/// answers — and the read has already applied it by the time the call site sees it. Casting again
/// there checked a `Node` against `Light`: the JVM rejected the method outright
/// (`VerifyError: Type 'Light' is not assignable to 'Node'`) and the native backend raised a
/// `ClassCastException`. `codegen/box/smartCasts/kt44814.kt` is the corpus case.
#[test]
fn a_member_read_on_narrowed_this_may_be_a_calls_receiver() {
    const SRC: &str = "\
class Node { fun tag(): Int = 7 }\n\
sealed class Base\n\
class Light(val node: Node) : Base()\n\
fun Base?.viaWhen(): Int = when (this) {\n\
    null -> 0\n\
    is Light -> node.tag()\n\
}\n\
fun Base?.viaIf(): Int { if (this is Light) return node.tag(); return 0 }\n\
fun Base.viaSubject(): Int = when (this) { is Light -> node.tag(); else -> 0 }\n\
fun box(): String {\n\
    val light: Base? = Light(Node())\n\
    if (light.viaWhen() != 7) return \"f1\"\n\
    if (light.viaIf() != 7) return \"f2\"\n\
    if (Light(Node()).viaSubject() != 7) return \"f3\"\n\
    if (null.viaWhen() != 0) return \"f4\"\n\
    if (null.viaIf() != 0) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("narrowed this member read as a receiver"),
        "OK"
    );
}

/// The same read, still narrowed when the call takes the narrowed `this`'s OTHER members as
/// arguments — the shape the corpus case is written in.
#[test]
fn a_narrowed_read_carries_its_siblings_as_arguments() {
    const SRC: &str = "\
class Tree\n\
class Node { fun children(t: Tree): Int = 3 }\n\
sealed class Base\n\
class Light(val node: Node, val tree: Tree) : Base()\n\
fun Base?.count(): Int = when (this) {\n\
    null -> -1\n\
    is Light -> node.children(tree)\n\
}\n\
fun box(): String {\n\
    val light: Base? = Light(Node(), Tree())\n\
    if (light.count() != 3) return \"f1\"\n\
    if (null.count() != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("narrowed read with sibling arguments"),
        "OK"
    );
}
