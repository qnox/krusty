//! A captured `var` of a reference type is read from its `Ref$ObjectRef.element` slot, an
//! `Object`. kotlinc casts it at the consumer: an `Any` parameter, local or return takes the element
//! unchanged, and only a consumer of the variable's own type (or another subtype of `Any`) casts.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
fun record(value: Any?) {}\n\
fun measure(text: String): Int = text.length\n\
fun describe(chars: CharSequence): Int = chars.length\n\
\n\
fun track(): Any? {\n\
    var label: String = \"a\"\n\
    val rename = { label = \"b\" }\n\
    rename()\n\
    record(label)\n\
    val held: Any = label\n\
    record(held)\n\
    measure(label)\n\
    describe(label)\n\
    return label\n\
}\n\
\n\
fun exact(): String {\n\
    var label = \"a\"\n\
    val rename = { label = \"OK\" }\n\
    rename()\n\
    return label\n\
}\n\
\n\
fun box(): String = if (track() == \"b\") exact() else \"fail\"\n\
";

#[test]
fn a_captured_reference_var_is_cast_at_its_consumer() {
    common::byte_diff_against_kotlinc_cp(
        "CapturedRefConsumer",
        SOURCE,
        "store/CapturedRefConsumerKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/CapturedRefConsumerKt differs from kotlinc: {diff}"));
}

#[test]
fn a_captured_reference_var_runs_like_kotlinc() {
    let actual = common::compile_and_run_box(
        SOURCE,
        "captured_ref_consumer",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
