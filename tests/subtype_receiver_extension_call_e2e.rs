//! A top-level extension declared on a supertype, called on a subtype receiver: `fun A.test()` on a
//! `B : A` value (`B().test()`). The checker resolves it through the receiver's supertype closure; the
//! lowerer must find the same registered extension by walking supertypes, not just the exact receiver
//! key. Same-file, runs on the JVM.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn extension_on_superclass_called_on_subclass() {
    const SRC: &str = "\
open class A\n\
class B : A()\n\
fun A.test() = 5\n\
fun box(): String = if (B().test() == 5) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("superclass extension on subclass"), "OK");
}

#[test]
fn extension_on_interface_called_on_implementor() {
    const SRC: &str = "\
interface I\n\
class C : I\n\
fun I.tag() = \"OK\"\n\
fun box(): String = C().tag()\n";
    assert_eq!(run(SRC).expect("interface extension on implementor"), "OK");
}

// NOTE: a NON-inline generic/`Any`-receiver extension called on a subtype
// (`fun <T> T.self()` / `fun Any.tag()` on `D()`) is a distinct, deliberately-unimplemented
// feature — the checker leaves it unresolved (it needs erased-`Object` boxing at the call, see the
// `generic_receiver_extensions` inline-only gate in `resolve.rs`). This suite covers only the
// concrete-supertype case that the checker already resolves; the generic case is tracked separately.
