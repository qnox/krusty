//! Interface default methods (`interface I { fun f() = "OK" }`) — a method with a body in an interface
//! is emitted as a JVM default method (concrete, non-abstract, non-final). An implementing class
//! inherits it or overrides it. Round-tripped under `-Xverify:all`.

use super::common;

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

/// kotlinc writes an interface member's body on `DefaultImpls`, and its `access$<name>$jd` bridge,
/// as a static that takes the interface instance as `$this` and the extension receiver as an
/// ordinary parameter `$receiver`: its local-variable row and its null check read `$receiver`, and
/// so does the forward a sub-interface republishes for an inherited member. A `$suspendImpl` that
/// takes a class's instance first names it the same way. krusty named it `$this$ie` and checked it
/// as `<this>`.
#[test]
fn a_default_impls_extension_receiver_is_named_receiver() {
    let source = "interface Face {\n\
        \x20   fun String.ie(n: Int): Int = length + n\n\
        \x20   val String.pe: Int get() = length\n\
        \x20   fun plain(y: Int): Int = y\n\
        }\n\
        interface Sub : Face\n\
        open class Open { open suspend fun String.se(n: Int): Int = n + length }\n";
    let classes = common::classes_against_kotlinc_module(&[("DefaultImplsReceiver.kt", source)]);
    for class in [
        "Face$DefaultImpls",
        "Face",
        "Sub$DefaultImpls",
        "Sub",
        "Open",
    ] {
        let (reference, krusty) = classes.class_listing(class);
        assert_eq!(
            krusty, reference,
            "{class}: kotlinc's code and debug tables"
        );
    }
}
