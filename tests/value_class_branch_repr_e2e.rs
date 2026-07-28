use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn elvis_coerces_each_value_class_branch() {
    const SRC: &str = "@JvmInline\n\
value class Meter(val value: Int)\n\
fun sum(first: Meter?, second: Meter?, fallback: Meter): Int {\n\
    val a = first ?: fallback\n\
    val b = first ?: second ?: fallback\n\
    return a.value + b.value\n\
}\n\
fun box(): String {\n\
    val result = sum(null, null, Meter(21))\n\
    return if (result == 42) \"OK\" else \"fail: $result\"\n\
}\n";
    assert_eq!(run(SRC).expect("value-class elvis branches"), "OK");
}

#[test]
fn nullable_join_boxes_value_class_arm() {
    const SRC: &str = "@JvmInline\n\
value class Meter(val value: Int)\n\
fun selected(): Meter {\n\
    val value = Meter(42) ?: null\n\
    return requireValue(value)\n\
}\n\
fun requireValue(value: Meter?): Meter = value!!\n\
fun box(): String = if (selected().value == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("nullable value-class join"), "OK");
}

#[test]
fn unsupported_value_class_return_shapes_are_rejected() {
    const LABELED: &str = "@JvmInline\n\
value class Meter(val value: Long)\n\
fun selected(): Meter? {\n\
    val value = Meter(2)\n\
    return run {\n\
        if (value.value > 0) return@run value\n\
        Meter(-1)\n\
    }\n\
}\n\
fun box(): String = if (selected()!!.value == 2L) \"OK\" else \"fail\"\n";
    assert_eq!(run(LABELED), None);

    const GENERIC_CAST: &str = "@JvmInline\n\
value class Wrapper<T : Int>(val value: T)\n\
fun <T : Int> read(value: Any?): Int = ((value as Wrapper<T>?) as Wrapper<T>).value\n\
fun box(): String = if (read<Int>(Wrapper(1)) == 1) \"OK\" else \"fail\"\n";
    assert_eq!(run(GENERIC_CAST), None);
}
