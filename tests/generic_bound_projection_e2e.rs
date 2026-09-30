//! A formal bounded by a use-site projection is inferred from the argument that
//! instantiates the bounded formal. `C : MutableCollection<in R>` plus
//! `MutableCollection<T>` fixes `R` to `T`, including when `T` is a caller type
//! parameter or the argument is a star.

use super::common;

fn assert_diagnostics(src: &str, expected: &[&str]) {
    let diagnostics =
        common::checker_diags_with_stdlib(src).expect("checker diagnostics available");
    assert_eq!(
        diagnostics.len(),
        expected.len(),
        "unexpected diagnostic count: {diagnostics:?}"
    );
    assert_eq!(diagnostics, expected);
}

#[test]
fn a_projected_bound_is_instantiated_from_the_argument() {
    const SRC: &str = "\
fun <R, C : MutableCollection<in R>> id(c: C): C = c\n\
fun <T> fromMutable(c: MutableCollection<T>): MutableCollection<T> = id(c)\n\
fun <T> fromArrayList(c: ArrayList<T>): ArrayList<T> = id(c)\n\
fun star(c: MutableCollection<*>): MutableCollection<*> = id(c)\n\
inline fun <reified T> Collection<*>.keep(): List<T> = filterIsInstanceTo(ArrayList<T>())\n\
fun box(): String {\n\
    val fromType = fromMutable(mutableListOf(\"a\"))\n\
    fromType.add(\"b\")\n\
    if (fromType != listOf(\"a\", \"b\")) return \"mutable:$fromType\"\n\
    val listed = fromArrayList(arrayListOf(1))\n\
    if (listed != listOf(1)) return \"array:$listed\"\n\
    val stars: MutableCollection<*> = star(mutableListOf(\"z\"))\n\
    if (stars.size != 1) return \"star\"\n\
    val kept: List<String> = listOf<Any>(\"a\", 1, \"b\").keep()\n\
    if (kept != listOf(\"a\", \"b\")) return \"keep:$kept\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "projected_bound");
}

#[test]
fn an_argument_outside_a_projected_bound_cannot_instantiate_it() {
    const SRC: &str = "\
fun <R, C : MutableCollection<in R>> id(c: C): C = c\n\
fun bad(n: Int) = id(n)\n";
    assert_diagnostics(
        SRC,
        &["argument type mismatch: actual type is 'Int', but 'MutableCollection<in Any>' was expected."],
    );
}
