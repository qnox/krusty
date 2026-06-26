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
fn companion_extends_no_arg_base_runs() {
    // A companion with a no-arg base CLASS: the registration pass now synthesizes the `super()` call,
    // so `C` used as a value is a `Base`.
    const SRC: &str = "open class Base { fun tag() = \"OK\" }\n\
class C { companion object : Base() }\n\
fun box(): String { val b: Base = C; return b.tag() }\n";
    assert_eq!(
        run(SRC).expect("companion extending a no-arg base compiles + runs"),
        "OK"
    );
}

#[test]
fn companion_extends_default_param_base_runs() {
    // The base ctor takes a parameter with a DEFAULT; the registration pass fills the default into the
    // synthesized `super(<default>)` call. (Base is referenced by a DIFFERENT class than the one whose
    // companion extends it — self-reference is a separate, unsupported shape.)
    const SRC: &str =
        "open class Base(val n: Int = 7) { fun tag() = if (n == 7) \"OK\" else \"x$n\" }\n\
class C { companion object : Base() }\n\
fun box(): String { val b: Base = C; return b.tag() }\n";
    assert_eq!(
        run(SRC).expect("companion extending a default-param base fills the default + runs"),
        "OK"
    );
}
