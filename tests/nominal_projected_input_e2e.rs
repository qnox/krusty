//! Applied nominal members retain the declaration input that an out/star projection closes.

use super::common;

#[test]
fn scalar_and_expanded_vararg_inputs_report_every_projected_receiver_error() {
    let source = r#"class Bucket<A> {
    fun put(value: A) {}
    fun many(vararg values: A) {}
}
fun closed(value: Bucket<out Number>) {
    value.put(1)
    value.many(2, 3)
}
fun star(value: Bucket<*>) {
    value.put(4)
    value.many(5)
}
"#;
    common::assert_errors_match_kotlinc(&[("Main.kt", source)], &[]);
}

#[test]
fn invariant_in_projected_named_default_and_shadowed_inputs_remain_accepted() {
    let source = r#"
class Bucket<A> {
    var seen: Int = 0
    fun put(value: A) { seen++ }
    fun many(prefix: String = "x", vararg values: A, after: Int = 1): Int =
        prefix.length + values.size + after
    fun <A> shadow(value: A): A = value
}
fun check(value: Bucket<Number>, input: Bucket<in Int>): Int {
    value.put(1)
    input.put(2)
    return value.many(values = arrayOf<Number>(1, 2), after = 3) +
        input.many("y", 3, 4, after = 4) + value.seen + input.seen
}
fun box(): String {
    val value = Bucket<Number>()
    val projected: Bucket<out Number> = value
    return if (check(value, Bucket<Number>()) == 15 && projected.shadow("OK") == "OK") "OK" else "wrong input"
}
"#;
    common::expect_box_same_as_kotlinc(source, "NominalProjectedInputs");
}

#[test]
fn a_projected_vararg_member_yields_to_an_applicable_extension() {
    let source = r#"
class Bucket<A> {
    fun many(vararg values: A): String = "member"
}
fun Bucket<*>.many(vararg values: Int): String = "extension"
fun box(): String {
    val projected: Bucket<out Number> = Bucket<Number>()
    val star: Bucket<*> = Bucket<Number>()
    return if (projected.many(1, 2) == "extension" && star.many(3) == "extension") "OK" else "wrong rung"
}
"#;
    common::expect_box_same_as_kotlinc(source, "NominalProjectedInputTower");
}

#[test]
fn nested_generic_producers_can_infer_bottom_for_closed_inputs() {
    let source = r#"
class Bucket<A> {
    fun put(value: A) {}
    fun many(vararg values: A) {}
}
fun <X> make(): X = throw IllegalStateException("bottom")
fun box(): String {
    val projected: Bucket<out Number> = Bucket<Number>()
    val star: Bucket<*> = Bucket<Number>()
    var caught = 0
    try { projected.put(make()) } catch (exception: IllegalStateException) { caught++ }
    try { projected.many(make()) } catch (exception: IllegalStateException) { caught++ }
    try { star.put(make()) } catch (exception: IllegalStateException) { caught++ }
    try { star.many(make()) } catch (exception: IllegalStateException) { caught++ }
    return if (caught == 4) "OK" else "missing bottom"
}
"#;
    common::expect_box_same_as_kotlinc(source, "NominalProjectedInputBottom");
}
