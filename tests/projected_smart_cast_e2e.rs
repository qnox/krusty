//! A `for` over `Array<out T>` or `Array<*>` binds the element as `T`, not the projection.
//! An `is` check then smart-casts that reference. Leaving the variable as `out Any` dropped the
//! cast, and the JVM rejected the narrowed members.

use super::common;

fn run(src: &str) {
    common::expect_box_same_as_kotlinc(src, "ProjectedSmartCast");
}

#[test]
fn nested_projected_arrays_smart_cast_to_a_progression() {
    const SRC: &str = "\
fun box(): String {\n\
    val sb = StringBuilder()\n\
    val metaArray = arrayOf(\n\
        arrayOf(1, 2, 3),\n\
        arrayOf(\"Hello\"),\n\
        arrayOf<Any>(),\n\
        arrayOf(1..10)\n\
    )\n\
    for (array in metaArray) {\n\
        for (elem in array) {\n\
            if (elem is IntProgression) {\n\
                for (i in elem) sb.append(i)\n\
            } else {\n\
                sb.append(elem)\n\
            }\n\
        }\n\
        sb.append('\\n')\n\
    }\n\
    val expected = \"123\\nHello\\n\\n12345678910\\n\"\n\
    if (sb.toString() != expected) return sb.toString()\n\
    return \"OK\"\n\
}\n";
    run(SRC);
}

#[test]
fn explicit_projection_smart_cast_reads_the_narrowed_member() {
    const SRC: &str = "\
fun starred(array: Array<*>): Int {\n\
    var n = 0\n\
    for (elem in array) {\n\
        if (elem is IntProgression) n += elem.first\n\
    }\n\
    return n\n\
}\n\
fun outAny(array: Array<out Any>): Int {\n\
    var n = 0\n\
    for (elem in array) {\n\
        if (elem is IntProgression) {\n\
            for (i in elem) n += i\n\
        }\n\
    }\n\
    return n\n\
}\n\
fun box(): String {\n\
    val values: Array<Any> = arrayOf(1..4, \"x\")\n\
    if (starred(values) != 1) return \"star\"\n\
    if (outAny(values) != 10) return \"out\"\n\
    return \"OK\"\n\
}\n";
    run(SRC);
}
