use super::common;

fn run_two(operator_source: &str, use_source: &str) -> Option<String> {
    common::compile_and_run_files_with_stdlib(&[
        ("Operators", operator_source),
        ("Use", use_source),
    ])
}

#[test]
fn cross_file_source_extension_unary_and_increment_operators_run() {
    if false /* toolchain gate panics */ || false
    /* toolchain gate panics */
    {
        return;
    }
    let operators = r#"
class Counter(val value: Int)
operator fun Counter.unaryMinus(): Counter = Counter(-value)
operator fun Counter.inc(): Counter = Counter(value + 1)
"#;
    let use_site = r#"
fun box(): String {
    var counter = Counter(2)
    if ((-counter).value != -2) return "unary"
    val old = counter++
    if (old.value != 2 || counter.value != 3) return "postfix"
    return if ((++counter).value == 4) "OK" else "prefix"
}
"#;
    assert_eq!(run_two(operators, use_site).as_deref(), Some("OK"));
}

#[test]
fn cross_file_inline_extension_operators_keep_callable_bodies() {
    if false /* toolchain gate panics */ || false
    /* toolchain gate panics */
    {
        return;
    }
    let operators = r#"
class InlineCounter(val value: Int)
inline operator fun InlineCounter.unaryMinus(): InlineCounter = InlineCounter(-value)
inline operator fun InlineCounter.inc(): InlineCounter = InlineCounter(value + 1)
"#;
    let use_site = r#"
fun box(): String {
    var counter = InlineCounter(2)
    if ((-counter).value != -2) return "unary"
    val old = counter++
    return if (old.value == 2 && counter.value == 3) "OK" else "increment"
}
"#;
    assert_eq!(run_two(operators, use_site).as_deref(), Some("OK"));
}
