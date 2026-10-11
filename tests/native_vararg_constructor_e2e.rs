//! A constructor taking a `vararg` parameter.
//!
//! A vararg parameter is PHYSICALLY an array, and the IR records it as one — so a constructor
//! taking it takes a reference like any other array parameter. The class model refused every such
//! class by name anyway, and there was nothing for the refusal to protect.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// The array the caller packs, read back from the class that kept it.
#[test]
fn a_vararg_constructor_parameter_arrives_as_the_array_it_is() {
    let source = "class Packed(val label: String, vararg val numbers: Int) {\n\
         \x20   fun total(): Int {\n\
         \x20       var sum = 0\n\
         \x20       for (n in numbers) sum += n\n\
         \x20       return sum\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val three = Packed(\"three\", 1, 2, 3)\n\
         \x20   if (three.numbers.size != 3) return \"fail size\"\n\
         \x20   if (three.numbers[1] != 2) return \"fail element\"\n\
         \x20   if (three.total() != 6) return \"fail total\"\n\
         \x20   val none = Packed(\"none\")\n\
         \x20   if (none.numbers.size != 0) return \"fail empty\"\n\
         \x20   if (none.label != \"none\") return \"fail the parameter before it\"\n\
         \x20   val spread = Packed(\"spread\", *intArrayOf(4, 5))\n\
         \x20   if (spread.total() != 9) return \"fail spread\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "VarargConstructor");
    expect_native_box(source, "VarargConstructor", "OK");
}

/// Through a SUPERCLASS constructor, which is where the refusal was most visible: a subclass
/// passing a different number of elements at each site.
#[test]
fn a_subclass_passes_its_own_elements_to_a_vararg_superclass_constructor() {
    let source = "open class Outer(val s: String, vararg val i: Int) {\n\
         \x20   class Inner : Outer(\"xyz\")\n\
         \x20   class Other : Outer(\"abc\", 1, 2, 3)\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Outer.Inner().i.size != 0) return \"fail none\"\n\
         \x20   if (Outer.Inner().s != \"xyz\") return \"fail label\"\n\
         \x20   if (Outer.Other().i.size != 3) return \"fail three\"\n\
         \x20   if (Outer.Other().i[2] != 3) return \"fail element\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "VarargSuperConstructor");
    expect_native_box(source, "VarargSuperConstructor", "OK");
}

/// And on an ENUM constant, whose constructor is reached from each entry rather than from a call.
#[test]
fn an_enum_entry_passes_its_own_elements_to_a_vararg_constructor() {
    let source = "enum class Piece(vararg val states: Int) {\n\
         \x20   I(3, 4, 5),\n\
         \x20   O(1),\n\
         \x20   NONE\n\
         }\n\
         fun box(): String {\n\
         \x20   if (Piece.I.states[0] != 3 || Piece.I.states.size != 3) return \"fail I\"\n\
         \x20   if (Piece.O.states.size != 1) return \"fail O\"\n\
         \x20   if (Piece.NONE.states.size != 0) return \"fail NONE\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "VarargEnumConstructor");
    expect_native_box(source, "VarargEnumConstructor", "OK");
}
