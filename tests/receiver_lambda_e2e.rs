//! A lambda passed to a USER function's RECEIVER function-type parameter (`Recv.() -> Unit`) binds its
//! implicit `this` to the receiver, so a bare member OR extension call inside resolves against it — the
//! `buildFoo { … }` DSL/builder shape. Previously the marker `TypeRef.fun_has_receiver` was parsed but
//! never consumed, so such a body's unqualified calls were "unresolved". Round-tripped on a real JVM.

use super::common;

#[test]
fn member_receiver_lambda_runs() {
    // `build { set(42); put(99) }` — `set` is a MEMBER, `put` an EXTENSION, both on the lambda's `this`.
    const SRC: &str = "class Box { var v: Int = 0; fun set(x: Int) { v = x } }\n\
fun Box.put(x: Int) { v = v + x }\n\
fun build(b: Box.() -> Unit): Box { val box = Box(); b(box); return box }\n\
fun box(): String {\n\
    val r = build { set(42); put(8) }\n\
    return if (r.v == 50) \"OK\" else \"FAIL ${r.v}\"\n\
}\n";
    common::assert_box_ok_with_stdlib(SRC, "B");
}
