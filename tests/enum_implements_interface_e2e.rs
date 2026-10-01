//! `enum class E : I` — an enum implementing an interface (the `implements` clause is emitted, so an
//! interface-typed call dispatches correctly). The abstract interface method is satisfied by the enum's
//! own method, by a per-entry override, or by a default. Generic interfaces (need erased bridges) and
//! unsatisfied abstract members skip cleanly. Round-tripped on the JVM via the INTERFACE type.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "E")
}

const ORDINAL_AND_NAME_SOURCE: &str = "interface Ordinaled { val ordinal: Int }\n\
interface Named { val name: String }\n\
enum class E : Ordinaled, Named { X }\n\
fun box(): String {\n\
    val ordinaled: Ordinaled = E.X\n\
    val named: Named = E.X\n\
    if (ordinaled.ordinal != 0) return \"fail ordinal\"\n\
    if (named.name != \"X\") return \"fail name\"\n\
    return \"OK\"\n\
}\n";

fn exact_member<'a>(compiler: &str, rows: &'a [String], signature: &str) -> &'a str {
    let matches = rows
        .iter()
        .filter(|row| row.split_whitespace().nth(2) == Some(signature))
        .collect::<Vec<_>>();
    let [row] = matches.as_slice() else {
        panic!("{compiler}: expected exactly one {signature}, found {matches:?}")
    };
    row.as_str()
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
    assert_eq!(
        run(ORDINAL_AND_NAME_SOURCE).expect("enum interface ordinal and name compile and run"),
        "OK"
    );
}

/// The interface accessors are ordinary public methods with kotlinc's exact descriptors and flags,
/// and each body forwards to the inherited `Enum` method rather than reimplementing the property.
#[test]
fn enum_interface_accessors_match_kotlinc_abi_and_forwarding() {
    let built = common::compare_with_kotlinc_plugin(
        "EnumInterfaceAccessors",
        ORDINAL_AND_NAME_SOURCE,
        "E",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    let reference_members = common::member_table(&built.reference_bytes);
    let krusty_members = common::member_table(&built.krusty_bytes);

    for (signature, declaration) in [
        ("getOrdinal()I", "int getOrdinal();"),
        ("getName()Ljava/lang/String;", "java.lang.String getName();"),
    ] {
        assert_eq!(
            exact_member("krusty", &krusty_members, signature),
            exact_member("kotlinc", &reference_members, signature),
            "{signature}: declaration, descriptor, flags and generic signature"
        );

        let reference = common::method_instructions(&built.reference, declaration);
        let krusty = common::method_instructions(&built.krusty, declaration);
        assert!(!reference.is_empty(), "kotlinc declares no {declaration}");
        assert_eq!(
            krusty, reference,
            "{signature}: exact forwarding instructions and selected inherited target"
        );
    }
}

#[test]
fn default_method_via_interface() {
    const SRC: &str = "interface HasV { fun v(): String = \"OK\" }\n\
enum class E : HasV { A }\n\
fun box(): String { val x: HasV = E.A; return x.v() }\n";
    assert_eq!(run(SRC).expect("default-method enum compiles + runs"), "OK");
}
