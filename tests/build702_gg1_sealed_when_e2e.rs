//! build.702 gg1: an exhaustive `when` over a CLASSPATH `sealed` class with NO explicit `else`, used as a
//! value (`fun f(d: D): Int = when (d) { is D.A -> d.n; is D.B -> 0 }`, `D` a classpath sealed). Kotlin
//! makes such a `when` exhaustive (and therefore an expression) by covering every sealed subtype; krusty
//! only treated a `when` as an expression when it had an explicit `else` OR the subject was a SAME-MODULE
//! sealed class, so a classpath sealed subject typed the `when` as `Unit` → "return type mismatch: expected
//! 'Int', actual 'Unit'".
//!
//! `when_sealed_exhaustive` now reads a classpath sealed class's direct subclasses from its `@Metadata`
//! (`Class.sealedSubclassFqName`, via `class_sealed_subclasses` / `SymbolSource::sealed_subclasses`), so an
//! exhaustive classpath `when` is proven an expression the same way a same-module one is. A NON-exhaustive
//! `when` used as a value still errors.
use super::common;

fn run(tag: &str, lib: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let libout = common::compile_lib(tag, lib)?;
    common::compile_and_run_box(main, "Main", &[libout, sl], Some(&jdk))
}

#[test]
fn exhaustive_when_over_classpath_sealed_as_value() {
    const LIB: &str = "package lib\n\
        sealed class D { class A(val n: Int) : D()\n class B : D() }\n";
    const MAIN: &str = "import lib.D\n\
        fun f(d: D): Int = when (d) { is D.A -> d.n; is D.B -> 0 }\n\
        fun box(): String {\n\
        \x20 val r = f(D.A(7)) + f(D.B())\n\
        \x20 return if (r == 7) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(
        run("gg1", LIB, MAIN).expect("exhaustive classpath sealed when"),
        "OK"
    );
}

#[test]
fn exhaustive_when_over_classpath_sealed_three_subclasses() {
    const LIB: &str = "package lib\n\
        sealed class D { class A(val n: Int) : D()\n class B : D()\n class C : D() }\n";
    const MAIN: &str = "import lib.D\n\
        fun f(d: D): Int = when (d) { is D.A -> d.n; is D.B -> 1; is D.C -> 2 }\n\
        fun box(): String {\n\
        \x20 val r = f(D.A(5)) + f(D.B()) + f(D.C())\n\
        \x20 return if (r == 8) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(
        run("gg1_3", LIB, MAIN).expect("3-subclass exhaustive"),
        "OK"
    );
}

#[test]
fn non_exhaustive_when_over_classpath_sealed_still_rejected() {
    // A `when` missing a subtype (`D.B`) is NOT exhaustive — used as a value it must NOT type-check (the fix
    // only promotes provably-exhaustive `when`s to expressions).
    let Some(diags) = common::checker_diags_against(
        "gg1_neg",
        "package lib\nsealed class D { class A(val n: Int) : D()\n class B : D() }\n",
        "import lib.D\nfun f(d: D): Int = when (d) { is D.A -> d.n }\nfun box(): String = \"OK\"\n",
    ) else {
        return;
    };
    assert!(
        !diags.is_empty(),
        "a non-exhaustive `when` used as a value must be rejected, got no diagnostics"
    );
}
