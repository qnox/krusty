//! `enum class E : I` — an enum implementing an interface (the `implements` clause is emitted, so an
//! interface-typed call dispatches correctly). The abstract interface method is satisfied by the enum's
//! own method, by a per-entry override, or by a default. Generic interfaces (need erased bridges) and
//! unsatisfied abstract members skip cleanly. Round-tripped on the JVM via the INTERFACE type.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "E", &[sl], Some(&jdk))
}

#[test]
fn enum_level_override_via_interface() {
    const SRC: &str = "interface HasV { fun v(): String }\n\
enum class E : HasV { A; override fun v() = \"OK\" }\n\
fun box(): String { val x: HasV = E.A; return x.v() }\n";
    assert_eq!(run(SRC).expect("enum-level override compiles + runs"), "OK");
}

#[test]
fn per_entry_override_via_interface() {
    const SRC: &str = "interface HasV { fun v(): String }\n\
enum class E : HasV { A { override fun v() = \"O\" }, B { override fun v() = \"K\" } }\n\
fun box(): String { val x: HasV = E.A; val y: HasV = E.B; return x.v() + y.v() }\n";
    assert_eq!(run(SRC).expect("per-entry override compiles + runs"), "OK");
}

#[test]
fn default_method_via_interface() {
    const SRC: &str = "interface HasV { fun v(): String = \"OK\" }\n\
enum class E : HasV { A }\n\
fun box(): String { val x: HasV = E.A; return x.v() }\n";
    assert_eq!(run(SRC).expect("default-method enum compiles + runs"), "OK");
}
