//! `x in a..b` where `a`/`b` are USER types desugars to `a.rangeTo(b).contains(x)`. A member
//! `operator fun rangeTo` returning a range type with a member `operator fun contains` is emitted as the
//! two operator calls; `!in` negates. Same-file, runnable. (An extension `rangeTo`/`contains` is a later
//! slice and still skips.)
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn user_class_member_range_in_operator() {
    const SRC: &str = "class VR(val a: Int, val b: Int) {\n\
        \x20 operator fun contains(v: V): Boolean = v.x in a..b\n\
        }\n\
        class V(val x: Int) {\n\
        \x20 operator fun rangeTo(o: V): VR = VR(x, o.x)\n\
        }\n\
        fun box(): String {\n\
        \x20 val inside = V(2) in V(1)..V(3)\n\
        \x20 val outside = V(5) !in V(1)..V(3)\n\
        \x20 return if (inside && outside) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("user member range in"), "OK");
}

#[test]
fn extension_range_in_operator() {
    // `rangeTo` is a same-module EXTENSION operator; `contains` is a member of the range type.
    const SRC: &str =
        "class VR(val a: Int, val b: Int) { operator fun contains(v: V): Boolean = v.x in a..b }\n\
        class V(val x: Int)\n\
        operator fun V.rangeTo(o: V): VR = VR(x, o.x)\n\
        fun box(): String = if (V(2) in V(1)..V(3) && V(5) !in V(1)..V(3)) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("extension range in"), "OK");
}
