//! Unqualified references to a sibling nested TYPE within the enclosing class body — a parameter type
//! (`fun m(i: Inner)`), a local `val v: Inner`, etc. — resolve to `Outer$Inner` (Kotlin nested-type
//! scoping). Construction was already handled; this covers TYPE positions.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

#[test]
fn nested_type_as_parameter_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun m(i: Inner): String = i.s\n\
    fun go(): String = m(Inner(\"OK\"))\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as parameter"), "OK");
}

#[test]
fn nested_type_as_local_var_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun go(): String { val v: Inner = Inner(\"OK\"); return v.s }\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as local var"), "OK");
}

#[test]
fn nested_type_as_is_smartcast_target() {
    // `is Inner` (and the smart-cast) narrows `a` to the nested type, so `a.v()` resolves.
    const SRC: &str = "class Outer {\n\
    class Inner { fun v() = \"OK\" }\n\
    fun m(a: Any): String = if (a is Inner) a.v() else \"F\"\n\
    fun go(): String = m(Inner())\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as is-smartcast"), "OK");
}

#[test]
fn nested_type_as_cast_target() {
    // `as Inner` on a nested type.
    const SRC: &str = "class Outer {\n\
    class Inner { fun v() = \"OK\" }\n\
    fun m(a: Any): String = (a as Inner).v()\n\
    fun go(): String = m(Inner())\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as cast target"), "OK");
}

#[test]
fn nested_type_as_return_type() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String)\n\
    fun mk(): Inner = Inner(\"OK\")\n\
    fun go(): String = mk().s\n\
}\n\
fun box(): String = Outer().go()\n";
    assert_eq!(run(SRC).expect("nested type as return"), "OK");
}
