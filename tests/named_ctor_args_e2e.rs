//! Named arguments on a constructor call (`C(b = 9)`), including ones that skip a leading parameter
//! whose default is a simple literal — the same name→position mapping top-level functions already get.
//! Round-tripped on the JVM.

mod common;

fn run(src: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    common::compile_and_run_box(src, "P", &[sl], Some(&jdk))
}

fn toolchain_ready() -> bool {
    common::java_home().is_some() && common::stdlib_jar().is_some()
}

#[test]
fn named_constructor_args_map_and_fill_defaults() {
    if !toolchain_ready() {
        return;
    }
    const SRC: &str = "// WITH_STDLIB\n\
class C(val a: Int = 1, val b: Int = 2, val c: Int = 3)\n\
fun box(): String {\n\
    val x = C(b = 9)\n\
    if (x.a != 1 || x.b != 9 || x.c != 3) return \"fail x\"\n\
    val y = C(1, c = 7)\n\
    if (y.a != 1 || y.b != 2 || y.c != 7) return \"fail y\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("named ctor args should compile + run"),
        "OK"
    );
}

#[test]
fn named_ctor_call_targets_primary_not_a_colliding_secondary() {
    if !toolchain_ready() {
        return;
    }
    // A named call (`C(b = 9)`) references the PRIMARY ctor's parameter names — it must NOT be routed
    // to a same-arity secondary ctor that only coincides on argument types. (Regression: the lowering
    // picked the `constructor(x: Int)` secondary, yielding `a = 9` instead of the default `a = 1`.)
    const SRC: &str = "// WITH_STDLIB\n\
class C(val a: Int = 1, val b: Int = 2) {\n\
    constructor(x: Int) : this(x, x)\n\
}\n\
fun box(): String {\n\
    val c = C(b = 9)\n\
    return if (c.a == 1 && c.b == 9) \"OK\" else \"fail: a=${c.a} b=${c.b}\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("named ctor vs secondary should compile + run"),
        "OK"
    );
}
