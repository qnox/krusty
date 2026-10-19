//! A classpath inline function whose result type is a bounded type parameter returns its bound
//! (`T : Throwable` as `Throwable`). Calling a member of the substituted type on that result casts
//! it to that type first, as kotlinc does; without the cast the verifier rejects the call.

use super::common;

const SOURCE: &str = "import kotlin.test.assertFailsWith\n\
\n\
fun reified(): String? =\n\
    assertFailsWith<UnsupportedOperationException> { throw UnsupportedOperationException(\"a\") }.message\n\
\n\
fun classed(): String? =\n\
    assertFailsWith(UnsupportedOperationException::class) { throw UnsupportedOperationException(\"b\") }.message\n\
\n\
fun box(): String = if (reified() + classed() == \"ab\") \"OK\" else \"fail\"\n";

fn classpath() -> Vec<std::path::PathBuf> {
    let kotlin_test = common::kotlin_test_jar()
        .expect("no kotlin-test jar found; provision the reference toolchain (`just` fetches it)");
    vec![common::stdlib_jar(), kotlin_test]
}

/// The calls verify and return the caught exceptions' messages.
#[test]
fn a_member_call_on_an_erased_bound_result_runs() {
    let mut classpath = classpath();
    let jdk = common::jdk_modules();
    classpath.push(jdk.clone());
    assert_eq!(
        common::expect_box_run(
            SOURCE,
            "InlineErasedBoundReceiver",
            &classpath,
            Some(jdk.as_path())
        ),
        "OK"
    );
}
