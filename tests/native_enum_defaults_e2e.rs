//! An enum CONSTANT leaving a constructor argument out.
//!
//! It is the same omission an ordinary construction makes, written a third way: `RED` where the
//! enum's constructor declares `(val rgb: Int = 0)` is not an expression and not a supertype call,
//! so neither of the collectors that find omissions saw it and every such enum declined.
//!
//! The constant reaches the class's own defaults wrapper — the one `Foo()` written as an
//! expression reaches — which fills the frame and runs the constructor. What the entry supplies is
//! then the parameters it did NOT omit.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, expect_native_decline};

/// Constants omitting one argument, two, and none — the defaults read in declaration order.
#[test]
fn an_enum_constant_reaches_its_constructors_defaults() {
    let source = "enum class Colour(val name2: String = \"?\", val rgb: Int = 7) {\n\
         \x20   RED(\"red\", 1),\n\
         \x20   GREEN(\"green\"),\n\
         \x20   PLAIN\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Colour.RED.name2 != \"red\" || Colour.RED.rgb != 1) return \"fail RED\"\n\
         \x20   if (Colour.GREEN.name2 != \"green\" || Colour.GREEN.rgb != 7) return \"fail GREEN\"\n\
         \x20   if (Colour.PLAIN.name2 != \"?\" || Colour.PLAIN.rgb != 7) return \"fail PLAIN\"\n\
         \x20   if (Colour.values().size != 3) return \"fail values\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "EnumConstructorDefaults");
    expect_native_box(source, "EnumConstructorDefaults", "OK");
}

/// A default that READS an earlier parameter, which is why the wrapper fills the frame in
/// declaration order rather than evaluating each default on its own.
#[test]
fn an_enum_default_reads_the_parameter_declared_before_it() {
    let source = "enum class Step(val base: Int, val doubled: Int = base * 2) {\n\
         \x20   ONE(1),\n\
         \x20   TWO(2, 5)\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Step.ONE.doubled != 2) return \"fail ONE\"\n\
         \x20   if (Step.TWO.doubled != 5) return \"fail TWO\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "EnumDefaultReadsEarlier");
    expect_native_box(source, "EnumDefaultReadsEarlier", "OK");
}

/// A constant with a BODY that also omits an argument still declines.
///
/// Its instance is a synthesized subclass whose own constructor takes the entry's arguments, while
/// the defaults are recorded against the ENUM — so the wrapper the other constants reach has
/// nothing to read for it.
#[test]
fn an_enum_constant_with_a_body_omitting_an_argument_still_declines() {
    expect_native_decline(
        "enum class Op(val label: String = \"none\") {\n\
         \x20   PLUS {\n\
         \x20       override fun show(): String = label\n\
         \x20   };\n\
         \x20   abstract fun show(): String\n\
         }\n\
         fun box(): String = Op.PLUS.show()\n",
        "EnumBodyDefault",
        "omitting a constructor argument",
    );
}
