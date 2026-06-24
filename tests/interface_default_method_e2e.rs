//! Interface default methods (`interface I { fun f() = "OK" }`) — a method with a body in an interface
//! is emitted as a JVM default method (concrete, non-abstract, non-final). An implementing class
//! inherits it or overrides it. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "C", &[sl], Some(&jdk))
}

#[test]
fn interface_default_method_inherited_and_overridden() {
    // The default method `greet()` is inherited (called via the interface type) by `En`, and overridden
    // by `Loud`. (Calling an inherited default through the *concrete* type — `En().greet()` — is a
    // separate follow-up: resolving an inherited-default call on the concrete class.)
    const SRC: &str = "interface Greeter {\n\
    fun greet(): String = \"hi\"\n\
}\n\
class En : Greeter\n\
class Loud : Greeter {\n\
    override fun greet() = \"HI\"\n\
}\n\
fun box(): String {\n\
    val e: Greeter = En()\n\
    if (e.greet() != \"hi\") return \"fail inherit: \" + e.greet()\n\
    if (En().greet() != \"hi\") return \"fail concrete: \" + En().greet()\n\
    if (Loud().greet() != \"HI\") return \"fail override: \" + Loud().greet()\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("interface default method should compile + run");
    assert_eq!(out, "OK");
}
