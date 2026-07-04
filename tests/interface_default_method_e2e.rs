//! Interface default methods (`interface I { fun f() = "OK" }`) — a method with a body in an interface
//! is emitted as a JVM default method (concrete, non-abstract, non-final). An implementing class
//! inherits it or overrides it. Round-tripped under `-Xverify:all`.

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
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

#[test]
fn default_method_reads_abstract_property() {
    // Corpus traits/genericMethod shape: a default method reads an abstract interface property — it
    // must route through the getter (invokeinterface), not a (nonexistent) interface field.
    const SRC: &str = "interface Named {\n\
    val who: String\n\
    fun hello(): String = \"hi \" + who\n\
}\n\
class P(override val who: String) : Named\n\
fun box(): String {\n\
    val p: Named = P(\"k\")\n\
    if (p.hello() != \"hi k\") return \"fail: \" + p.hello()\n\
    if (P(\"c\").hello() != \"hi c\") return \"fail concrete\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("default method reading an abstract property should compile + run");
    assert_eq!(out, "OK");
}
