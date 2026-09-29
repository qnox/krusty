//! `enum class E : I` — an enum implementing an interface (the `implements` clause is emitted, so an
//! interface-typed call dispatches correctly). The abstract interface method is satisfied by the enum's
//! own method, by a per-entry override, or by a default. Generic interfaces (need erased bridges) and
//! unsatisfied abstract members skip cleanly. Round-tripped on the JVM via the INTERFACE type.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "E")
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

/// `Enum.ordinal`/`name` are JVM methods `ordinal()`/`name()`, while an interface property of the
/// same spelling is `getOrdinal()`/`getName()`. The enum must bridge the interface accessor to the
/// inherited method.
#[test]
fn enum_inherits_interface_ordinal_and_name() {
    const SRC: &str = "interface Ordinaled { val ordinal: Int }\n\
interface Named { val name: String }\n\
enum class E : Ordinaled, Named { X }\n\
fun box(): String {\n\
    val ordinaled: Ordinaled = E.X\n\
    val named: Named = E.X\n\
    if (ordinaled.ordinal != 0) return \"fail ordinal\"\n\
    if (named.name != \"X\") return \"fail name\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("enum interface ordinal and name compile and run"),
        "OK"
    );
}

#[test]
fn default_method_via_interface() {
    const SRC: &str = "interface HasV { fun v(): String = \"OK\" }\n\
enum class E : HasV { A }\n\
fun box(): String { val x: HasV = E.A; return x.v() }\n";
    assert_eq!(run(SRC).expect("default-method enum compiles + runs"), "OK");
}
