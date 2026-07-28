//! Use-site variance projections in generic type arguments.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
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
fn nested_in_projection() {
    // `Box<in Box<String>>` — `in` before a nested generic.
    const SRC: &str = "class Box<T>(val v: T)\n\
fun f(b: Box<in Box<String>>): String = \"OK\"\n\
fun box(): String = f(Box(Box(\"x\")))\n";
    assert_eq!(run(SRC).expect("nested in projection"), "OK");
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
fn unresolved_projected_return_remains_gated() {
    const SRC: &str = "class Context<T>\n\
fun <T> select(context: Context<in T>): T = throw IllegalStateException()\n\
fun box(): String {\n\
    select(Context<Any>())\n\
    return \"OK\"\n\
}\n";
    assert!(
        run(SRC).is_none(),
        "an unconstrained projected return must not be lowered as an arbitrary type"
    );
}

#[test]
fn nothing_projected_return_remains_gated() {
    const SRC: &str = "class Context<T>\n\
fun <T> something(): T = Any() as T\n\
fun <T> Any.decodeIn(context: Context<in T>): T = something()\n\
fun <T> Any?.decodeOut(context: Context<out T>): T =\n\
    this?.decodeIn(context) ?: throw AssertionError()\n\
fun box(): String {\n\
    \"value\".decodeOut(Context<Any>())\n\
    return \"OK\"\n\
}\n";
    assert!(run(SRC).is_none());
}
