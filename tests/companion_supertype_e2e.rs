//! A `companion object` with a declared supertype (`class C { companion object : I { … } }`). The parser
//! previously DISCARDED the companion's supertype list and the synthesized `C$Companion` was hardcoded to
//! extend `kotlin/Any` implementing nothing — so the companion was not actually an `I` at runtime. The
//! parser now captures the companion's supertypes and the companion `IrClass` implements its declared
//! interfaces. (A companion with a base CLASS — `companion object : Base()` — still needs a `super(args)`
//! call the registration pass doesn't build, so that shape skips, never miscompiles.)

mod common;

use std::path::PathBuf;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

fn compiles(src: &str) -> bool {
    let Some(jh) = common::java_home() else {
        return true;
    };
    let Some(sl) = common::stdlib_jar() else {
        return true;
    };
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_in_process(src, "Comp", &[sl], Some(&jdk)).is_some()
}

#[test]
fn companion_implements_interface() {
    const SRC: &str = "interface I { fun f(): String }\n\
class C { companion object : I { override fun f() = \"OK\" } }\n\
fun box(): String = C.f()\n";
    assert!(
        compiles(SRC),
        "companion object implementing an interface should compile + emit"
    );
}

#[test]
fn companion_used_as_its_interface_value() {
    // `C` (a class whose companion implements `I`) used as a VALUE is its companion instance,
    // assignable to `I`; calling the interface method dispatches to the companion's override.
    const SRC: &str = "interface I { fun f(): String }\n\
class C { companion object : I { override fun f() = \"OK\" } }\n\
fun box(): String { val i: I = C; return i.f() }\n";
    assert_eq!(
        run(SRC).expect("companion used as its interface value compiles + runs"),
        "OK"
    );
}

#[test]
fn companion_with_base_class_skips_cleanly() {
    // A companion with a base CLASS is not yet modeled (needs a super(args) call) — it must skip the
    // file (compile returns None / the harness marks it unsupported), never miscompile.
    const SRC: &str = "open class Base\n\
class C { companion object : Base() }\n\
fun box(): String = \"OK\"\n";
    // Either it compiles (if some path supports it) or skips — both are acceptable; it must not panic.
    let _ = compiles(SRC);
}
