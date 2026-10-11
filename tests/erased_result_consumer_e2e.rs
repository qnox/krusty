//! A generic call's erased result is materialized at its consumer's type, as kotlinc's codegen
//! does: a function returning a supertype of the substituted type casts the erased `Object` to the
//! declared return type once (or not at all for `Any`), never to the substituted type first.
use super::common;

const SOURCE: &str = "class Box<T>(var v: T)\n\
fun any(b: Box<String>): Any = b.v\n\
fun chars(b: Box<String>): CharSequence = b.v\n\
fun comparable(b: Box<String>): Comparable<String> = b.v\n\
fun exact(b: Box<String>): String = b.v\n\
fun nullable(b: Box<String?>): Any? = b.v\n\
fun listed(l: List<String>): CharSequence { return l[0] }\n\
fun box(): String {\n\
\x20   val b = Box(\"OK\")\n\
\x20   if (any(b) != \"OK\" || chars(b) != \"OK\" || comparable(b) != \"OK\") return \"fail\"\n\
\x20   if (nullable(Box<String?>(null)) != null || listed(listOf(\"OK\")) != \"OK\") return \"fail\"\n\
\x20   return exact(b)\n\
}\n";

#[test]
fn a_returned_erased_result_is_cast_only_to_the_return_type() {
    common::byte_diff_against_kotlinc_cp(
        "ErasedResultConsumer",
        SOURCE,
        "ErasedResultConsumerKt",
        &[common::stdlib_jar()],
    )
    .expect("the reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("ErasedResultConsumerKt differs from kotlinc:\n{diff}"));
}

#[test]
fn a_returned_erased_result_runs_like_kotlinc() {
    let actual = common::compile_and_run_box(
        SOURCE,
        "erased_result_consumer",
        &[common::stdlib_jar()],
        Some(&common::jdk_modules()),
    )
    .expect("the source compiles and the JVM runner is provisioned");
    assert_eq!(actual, "OK");
}
