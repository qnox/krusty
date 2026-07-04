//! bb1: a classpath constructor called with an argument that is a NOMINAL SUBTYPE of the parameter
//! (`Outer(s: Sub)` called with a sealed/open subclass `Sub.U(…)`). The `<init>` overload resolution only
//! matched an exact / value-class-erased / JVM-collection-erased argument; a plain reference subtype (no
//! collection erasure, so `jvm_args == args`) skipped the subtype pass → `unresolved function 'Outer'`.
//! `resolve_constructor` now has a general nominal-subtype fallback (walk each argument's classpath
//! supertype closure to its parameter) after the exact matches, so the most-specific ctor still wins and a
//! scalar parameter is never coerced. Runnable end-to-end.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let libout = common::compile_lib(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(&jdk))
}

#[test]
fn sealed_subclass_ctor_as_constructor_argument() {
    // `Outer(Sub.U("OK"))` — a sealed nested subclass constructor call passed where the parent `Sub` is
    // expected. Construct, then read the value back through a cast.
    const LIB: &str = "package lib\n\
        sealed class Sub { class U(val v: String) : Sub() }\n\
        class Outer(val s: Sub)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.Sub\n\
        fun box(): String {\n\
        \x20 val o = Outer(Sub.U(\"OK\"))\n\
        \x20 return (o.s as Sub.U).v\n\
        }\n";
    assert_eq!(
        run("bb1_sealed", LIB, MAIN).expect("sealed subclass ctor arg"),
        "OK"
    );
}

#[test]
fn sealed_subclass_ctor_as_named_argument() {
    // The named-argument form `Outer(s = Sub.U(…))` resolves the same way.
    const LIB: &str = "package lib\n\
        sealed class Sub { class U(val v: String) : Sub() }\n\
        class Outer(val s: Sub)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.Sub\n\
        fun box(): String {\n\
        \x20 val o = Outer(s = Sub.U(\"OK\"))\n\
        \x20 return (o.s as Sub.U).v\n\
        }\n";
    assert_eq!(
        run("bb1_named", LIB, MAIN).expect("sealed subclass named ctor arg"),
        "OK"
    );
}

#[test]
fn open_subclass_ctor_as_constructor_argument() {
    // Not sealed-specific — any nominal subtype (`open class Sub`, `U : Sub()`).
    const LIB: &str = "package lib\n\
        open class Sub\n\
        class U(val v: String) : Sub()\n\
        class Outer(val s: Sub)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.U\n\
        fun box(): String {\n\
        \x20 val o = Outer(U(\"OK\"))\n\
        \x20 return (o.s as U).v\n\
        }\n";
    assert_eq!(
        run("bb1_open", LIB, MAIN).expect("open subclass ctor arg"),
        "OK"
    );
}

#[test]
fn exact_type_argument_still_resolves() {
    // Guard: the exact-type constructor argument path (parameter type == argument type) is unchanged.
    const LIB: &str = "package lib\n\
        class Sub(val v: String)\n\
        class Outer(val s: Sub)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.Sub\n\
        fun box(): String {\n\
        \x20 val o = Outer(Sub(\"OK\"))\n\
        \x20 return o.s.v\n\
        }\n";
    assert_eq!(run("bb1_exact", LIB, MAIN).expect("exact ctor arg"), "OK");
}
