//! Use-site variance projections in generic type arguments.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn generic_supertype_arguments_are_applied_before_variance_is_checked() {
    const SRC: &str = "interface Source<out T>\n\
class Exact<T>(private val value: T) : Source<T>\n\
fun consume(source: Source<Any>): String = if (source is Exact<*>) \"OK\" else \"fail\"\n\
fun box(): String = consume(Exact(\"value\"))\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "AppliedGenericSupertype"),
        "OK"
    );
}

#[test]
fn contravariant_nested_array_accepts_a_star_projected_element() {
    const SRC: &str = "fun replace(values: Array<in Array<String>>) {\n\
    values[0] = arrayOf(\"OK\")\n\
}\n\
fun box(): String {\n\
    val values: Array<Array<*>> = arrayOf(arrayOf(1))\n\
    replace(values)\n\
    return values[0][0] as String\n\
}\n";
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "NestedStarProjection"),
        "OK"
    );
}

#[test]
fn in_and_out_projections() {
    const SRC: &str = "class Box<T>(val v: T)\n\
fun unwrap(b: Box<out Any>): Any = b.v\n\
fun put(b: Box<in String>): String = \"OK\"\n\
fun box(): String {\n\
    val b: Box<out String> = Box(\"OK\")\n\
    if (put(Box(\"x\")) != \"OK\") return \"f1\"\n\
    return unwrap(b) as String\n\
}\n";
    assert_eq!(run(SRC).expect("in/out projections parse + run"), "OK");
}

#[test]
fn in_projection_is_read_through_its_upper_bound() {
    const SRC: &str = "class Box<T>\n\
fun take(value: Box<out Any?>) {}\n\
fun <T> read(value: Box<out T>): T = null as T\n\
fun accepted(value: Box<in String>) { take(value) }\n\
fun rejected(value: Box<in String>): String = read(value)\n";
    let (code, diagnostics) = common::kotlinc_source_result("InProjectionReadBound", SRC);
    assert_ne!(
        code, 0,
        "kotlinc unexpectedly inferred String: {diagnostics}"
    );
    let ours = common::front_end_diagnostics(SRC, &[], None);
    assert_eq!(ours.len(), 1, "unexpected diagnostics: {ours:?}");
    assert!(ours[0].contains("return type mismatch"), "{ours:?}");
}

#[test]
fn contravariant_only_call_infers_nothing_before_lowering() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(value: Context<in T>): T = null as T\n\
fun unused() { select(Context<Any>()) }\n\
fun box(): String = \"OK\"\n";
    let (code, diagnostics) = common::kotlinc_source_result("ContravariantOnlyNothing", SRC);
    assert_eq!(code, 0, "kotlinc rejected the call: {diagnostics}");
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "ContravariantOnlyNothing"),
        "OK"
    );
}

#[test]
fn expected_result_completes_a_contravariant_only_constraint() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(value: Context<in T>): T = \"OK\" as T\n\
fun box(): String = select(Context<Any>())\n";
    let (code, diagnostics) = common::kotlinc_source_result("ExpectedProjectedResult", SRC);
    assert_eq!(code, 0, "kotlinc rejected the call: {diagnostics}");
    assert_eq!(
        common::expect_box_run_with_stdlib(SRC, "ExpectedProjectedResult"),
        "OK"
    );
}

#[test]
fn invariant_argument_binding_is_not_overwritten_by_expected_result() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(value: Context<T>): T = \"OK\" as T\n\
fun box(): String = select(Context<Any>())\n";
    let (code, _) = common::kotlinc_source_result("InvariantExpectedResult", SRC);
    assert_ne!(code, 0, "kotlinc unexpectedly overwrote invariant T = Any");
    assert!(!common::front_end_diagnostics(SRC, &[], None).is_empty());
}

#[test]
fn nested_in_projection() {
    // `Box<in Box<String>>` — `in` before a nested generic.
    const SRC: &str = "class Box<T>(val v: T)\n\
fun f(b: Box<in Box<String>>): String = \"OK\"\n\
fun box(): String = f(Box(Box(\"x\")))\n";
    assert_eq!(run(SRC).expect("nested in projection"), "OK");
}

#[test]
fn conflicting_declaration_and_use_site_variance_is_rejected() {
    const SRC: &str = "class Producer<out T>\nfun bad(value: Producer<in String>) = value\n";
    assert!(run(SRC).is_none());
}

#[test]
fn explicit_generic_argument_avoids_projected_inference_gate() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(context: Context<in T>, value: T): T = value\n\
fun box(): String = select<String>(Context<Any>(), \"OK\")\n";
    assert_eq!(
        run(SRC).expect("explicit projected generic call compiles"),
        "OK"
    );
}

#[test]
fn invariant_value_witness_avoids_projected_inference_gate() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(context: Context<in T>, value: T): T = value\n\
fun box(): String = select(Context<Any>(), \"OK\")\n";
    assert_eq!(
        run(SRC).expect("invariant value argument determines the inferred return"),
        "OK"
    );
}

#[test]
fn generic_extension_boxes_a_primitive_receiver() {
    const SRC: &str = "fun <T> T.id(): T = this\n\
fun box(): String = if (42.id() == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("generic extension call compiles"), "OK");
}

#[test]
fn covariant_parameter_infers_the_return_type() {
    const SRC: &str = "class Context<T>(val value: T)\n\
fun <T> read(context: Context<out T>): T = context.value\n\
fun box(): String = read(Context(\"OK\"))\n";
    assert_eq!(run(SRC).expect("covariant return inference"), "OK");
}

#[test]
fn expected_type_resolves_projected_return_inference() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(context: Context<in T>): T = throw IllegalStateException()\n\
fun box(): String {\n\
    if (1 == 2) {\n\
        val inferred: String = select(Context<Any>())\n\
        return inferred\n\
    }\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("the expected type determines the projected return"),
        "OK"
    );
}

#[test]
fn contravariant_only_result_can_be_discarded() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(context: Context<in T>): T = throw IllegalStateException()\n\
fun probe(run: Boolean) {\n\
    if (run) select(Context<Any>())\n\
}\n\
fun box(): String {\n\
    probe(false)\n\
    return \"OK\"\n\
}\n";
    assert_eq!(run(SRC).expect("Nothing result can be discarded"), "OK");
}

#[test]
fn nested_projected_nothing_result_can_be_discarded() {
    const SRC: &str = "class Context<T>\n\
fun <T> something(): T = Any() as T\n\
fun <T> Any.decodeIn(context: Context<in T>): T = something()\n\
fun <T> Any?.decodeOut(context: Context<out T>): T =\n\
    this?.decodeIn(context) ?: throw AssertionError()\n\
fun box(): String {\n\
    \"value\".decodeOut(Context<Any>())\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        run(SRC).expect("nested Nothing result can be discarded"),
        "OK"
    );
}
