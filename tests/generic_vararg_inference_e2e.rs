use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn vararg_args_bind_type_parameter_for_member_access() {
    const SRC: &str = "class Entry(val value: Int)\n\
fun <T> takeVarargs(vararg e: T): T {\n\
    return e[e.size - 1]\n\
}\n\
fun box(): String {\n\
    val a = takeVarargs(Entry(1), Entry(42))\n\
    return if (a.value == 42) \"OK\" else \"fail: ${a.value}\"\n\
}\n";
    assert_eq!(run(SRC).expect("vararg T inference"), "OK");
}

#[test]
fn vararg_value_class_args_bind_type_parameter() {
    const SRC: &str = "@JvmInline\n\
value class Token(val value: Int)\n\
fun <T> takeVarargs(vararg e: T): T {\n\
    return e[e.size - 1]\n\
}\n\
fun test(first: Token, second: Token): Int {\n\
    val a = takeVarargs(first, second)\n\
    return a.value\n\
}\n\
fun box(): String {\n\
    val r = test(Token(1), Token(42))\n\
    return if (r == 42) \"OK\" else \"fail: $r\"\n\
}\n";
    assert_eq!(run(SRC).expect("vararg VC T inference"), "OK");
}

#[test]
fn vararg_primitive_args_still_infer() {
    const SRC: &str = "fun <T> last(vararg e: T): T = e[e.size - 1]\n\
fun box(): String {\n\
    val x = last(1, 2, 3)\n\
    return if (x == 3) \"OK\" else \"fail: $x\"\n\
}\n";
    assert_eq!(run(SRC).expect("vararg prim inference"), "OK");
}

#[test]
fn generic_vararg_spread_runs() {
    const SRC: &str = "class Entry(val text: String)\n\
fun <T> last(vararg values: T): T = values[values.size - 1]\n\
fun box(): String {\n\
    val values = arrayOf(Entry(\"OK\"))\n\
    return last(*values).text\n\
}\n";
    let (reference_code, stderr) = common::kotlinc_source_result("GenericVarargSpread", SRC);
    assert_eq!(reference_code, 0, "kotlinc rejected fixture: {stderr}");
    assert_eq!(run(SRC).as_deref(), Some("OK"));
}

#[test]
fn generic_value_class_storage_runs() {
    const SRC: &str = "@JvmInline\n\
value class Wrapper<T : Int>(val value: T)\n\
fun <T> last(vararg values: T): T = values[values.size - 1]\n\
fun <T : Int> read(first: Wrapper<T>, optional: Wrapper<T>?): Int {\n\
    val left = last(first)\n\
    val right = last(optional) ?: Wrapper(-1)\n\
    return left.value + right.value\n\
}\n\
fun box(): String = if (read(Wrapper(1), Wrapper(2)) == 3) \"OK\" else \"fail\"\n";
    assert_eq!(common::expect_box_run_with_stdlib(SRC, "Main"), "OK");
}
