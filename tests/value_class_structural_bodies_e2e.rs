//! The bodies of a value class's generated `hashCode-impl` and `equals-impl`.
//!
//! kotlinc hashes a primitive underlying through its wrapper's static `hashCode`
//! (`Integer.hashCode(I)`, `Short.hashCode(S)`, …) and an unsigned one through that class's own
//! `hashCode-impl`; krusty returned an `Int`/`Short`/`Byte`/`Char`/`UInt`-family value unchanged.
//!
//! `equals-impl` unboxes the other value into a temporary and guards on the negated comparison
//! `arg0 != tmp`; krusty compared into a boolean and branched on that, and for a reference
//! underlying passed the operands in the opposite order — `areEqual(other, arg0)` — which runs the
//! OTHER value's `equals`.
use super::common;
use super::serialization_companion_byte_parity_e2e::{
    compare_with_kotlinc_plugin, method_instructions,
};

#[test]
fn primitive_underlying_bodies_match_kotlinc() {
    for (class, underlying, carrier) in [
        ("I", "Int", "int"),
        ("L", "Long", "long"),
        ("B", "Boolean", "boolean"),
        ("D", "Double", "double"),
        ("F", "Float", "float"),
        ("S", "Short", "short"),
        ("Y", "Byte", "byte"),
        ("C", "Char", "char"),
        ("U", "UInt", "int"),
        ("UL", "ULong", "long"),
    ] {
        let source = format!("@JvmInline value class {class}(val v: {underlying})\n");
        let Some(built) = compare_with_kotlinc_plugin(
            &format!("ValueClassBodies{class}"),
            &source,
            class,
            &[common::stdlib_jar()],
            "25",
            &[],
        ) else {
            eprintln!("skipping: reference kotlinc or javap unavailable");
            return;
        };
        for member in [
            format!("int hashCode-impl({carrier})"),
            format!("boolean equals-impl({carrier}, java.lang.Object)"),
        ] {
            assert_eq!(
                method_instructions(&built.krusty, &member),
                method_instructions(&built.reference, &member),
                "{class}: {member}"
            );
        }
    }
}

#[test]
fn equality_runs_the_receivers_equals() {
    common::expect_box_ok_with_stdlib(
        "class Step(val n: Int) {\n\
         \x20   override fun equals(other: Any?) = other is Step && other.n == n + 1\n\
         \x20   override fun hashCode() = 0\n\
         }\n\
         @JvmInline value class G<T>(val v: T)\n\
         @JvmInline value class Small(val v: Short)\n\
         fun box(): String {\n\
         \x20   val one: Any = G(Step(1))\n\
         \x20   val two: Any = G(Step(2))\n\
         \x20   if (!one.equals(two)) return \"FAIL forward\"\n\
         \x20   if (two.equals(one)) return \"FAIL backward\"\n\
         \x20   if (Small(7).hashCode() != 7.toShort().hashCode()) return \"FAIL hash\"\n\
         \x20   return if (Small(7) == Small(7)) \"OK\" else \"FAIL small\"\n\
         }\n",
        "value-class equality order",
    );
}
