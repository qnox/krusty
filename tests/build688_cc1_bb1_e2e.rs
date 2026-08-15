//! build.688 cc1 + bb1.
//!
//! cc1: an `is`-check on a value from a SAME-PACKAGE classpath call (`package q`, no import) used as a plain
//! boolean value (`val v = Rules.validate(s); return v is V.Ok`). Lowering previously re-resolved `V.Ok`
//! but missed the file's implicit same-package import, so it erased to `Ty::Error` and bailed. The checker now
//! records the resolved `TypeRef`; lowering consumes that identity and performs no import lookup.
//!
//! bb1: a value-class parameter with a no-arg default combined with nominal-subtype arguments in ONE
//! constructor call (`data class Outer(id: Vid, a: A, b: B)` built `Outer(id = Vid(), a = A.X(…), b =
//! B.Y(…))`, `Vid` @JvmInline value class, `A`/`B` sealed). The value-class parameter makes the constructor a
//! synthetic `DefaultConstructorMarker` overload; `resolve_synthetic_constructor` matched arguments exactly,
//! and the plain nominal-subtype pass skipped the marker constructor (its trailing marker breaks the arity),
//! so a subclass argument left `Outer` unresolved. The synthetic-constructor matcher now also accepts a
//! reference nominal-subtype argument (`ctor_arg_subtype_of_param`).
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let libout = common::compile_lib(tag, lib)?;
    Some(common::expect_box_run(
        main,
        "Main",
        &[libout, sl],
        Some(jdk.as_path()),
    ))
}

#[test]
fn cc1_is_check_on_same_package_classpath_value() {
    // Both `lib` and the use site are `package q` — `V`/`V.Ok` are referenced unqualified.
    const LIB: &str = "package q\n\
        sealed class V { class Ok(val v: String) : V()\n class Err : V() }\n\
        object Rules { fun validate(s: String): V = V.Ok(s) }\n";
    const MAIN: &str = "package q\n\
        fun g(s: String): Boolean { val v = Rules.validate(s); return v is V.Ok }\n\
        fun box(): String = if (g(\"x\")) \"OK\" else \"fail\"\n";
    assert_eq!(
        run("cc1", LIB, MAIN).expect("is on same-package classpath value"),
        "OK"
    );
}

#[test]
fn bb1_value_class_default_with_subtype_ctor_args() {
    const LIB: &str = "package lib\n\
        @JvmInline value class Vid(val v: String = \"d\")\n\
        sealed class A { class X(val s: String) : A() }\n\
        sealed class B { class Y(val s: String) : B() }\n\
        data class Outer(val id: Vid, val a: A, val b: B)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.Vid\nimport lib.A\nimport lib.B\n\
        fun box(): String {\n\
        \x20 val o = Outer(id = Vid(), a = A.X(\"1\"), b = B.Y(\"2\"))\n\
        \x20 val ax = o.a as A.X\n\
        \x20 return if (o.id.v == \"d\" && ax.s == \"1\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("bb1", LIB, MAIN).expect("value-class default + subtype ctor args"),
        "OK"
    );
}

#[test]
fn bb1_positional_and_provided_value_class() {
    // Positional form + a PROVIDED (non-default) value-class argument, still with subtype args.
    const LIB: &str = "package lib\n\
        @JvmInline value class Vid(val v: String = \"d\")\n\
        sealed class A { class X(val s: String) : A() }\n\
        sealed class B { class Y(val s: String) : B() }\n\
        data class Outer(val id: Vid, val a: A, val b: B)\n";
    const MAIN: &str = "import lib.Outer\nimport lib.Vid\nimport lib.A\nimport lib.B\n\
        fun box(): String {\n\
        \x20 val o = Outer(Vid(\"z\"), A.X(\"1\"), B.Y(\"2\"))\n\
        \x20 return if (o.id.v == \"z\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        run("bb1p", LIB, MAIN).expect("provided vc + subtype args"),
        "OK"
    );
}
