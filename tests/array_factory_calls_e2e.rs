//! `kotlin` array factory calls are ordinary generic calls: a spread contributes its projected
//! element type, and an empty factory takes its element type from the parameter or declaration
//! that receives the result.

use super::common;

#[test]
fn empty_array_factories_take_their_element_type_from_the_receiving_position() {
    common::expect_box_ok_with_stdlib(
        "interface Item { val name: String }\n\
         open class Holder(val label: String, vararg val items: Item) {\n\
             fun render(): String = label + items.joinToString(\"\") { it.name }\n\
         }\n\
         class Spread : Holder(\"S\", *arrayOf())\n\
         class Secondary : Holder {\n\
             constructor() : super(\"C\", *emptyArray())\n\
         }\n\
         fun <T> sizeOf(values: Array<T>): Int = values.size\n\
         fun box(): String {\n\
             if (Spread().render() != \"S\") return \"empty spread: ${Spread().render()}\"\n\
             if (Secondary().render() != \"C\") return \"secondary: ${Secondary().render()}\"\n\
             val strings: Array<String> = arrayOf()\n\
             if (strings.size != 0) return \"contextual empty\"\n\
             if (sizeOf<String>(arrayOf()) != 0) return \"argument empty\"\n\
             val nulls = arrayOfNulls<String>(2)\n\
             if (nulls.size != 2 || nulls[0] != null || nulls[1] != null) return \"nulls\"\n\
             return \"OK\"\n\
         }\n",
        "EmptyArrayFactories",
    );
}

#[test]
fn spreads_contribute_their_projected_and_primitive_elements() {
    common::expect_box_ok_with_stdlib(
        "fun projected(flag: Boolean): Array<out Any> =\n\
             if (flag) arrayOf<String>(\"a\", \"b\") else arrayOf<Int>(1)\n\
         fun box(): String {\n\
             val spread = arrayOf(*projected(true), *projected(false), \"c\")\n\
             if (spread.size != 4 || spread[2] != 1 || spread[3] != \"c\") return \"projected spread\"\n\
             val ints = intArrayOf(1, *intArrayOf(2, 3))\n\
             if (ints.sum() != 6) return \"primitive spread\"\n\
             return \"OK\"\n\
         }\n",
        "ArrayFactorySpreads",
    );
}
