//! An `@OnlyInputTypes` variable may be solved by a result-only producer's completed type.
//!
//! `assertEquals(emptyList(), calls)` with `calls: MutableList<String>` solves `T = List<String>`.
//! That is an input type: `emptyList()` is completed to `List<String>`, the type it has once the
//! call is solved. The witness search only saw its provisional `List<T>`, so neither occurrence
//! admitted both arguments and every `assertEquals` overload was rejected.

use super::common;

const SOURCE: &str = r#"
import kotlin.test.assertEquals

class Recorder {
    val calls = mutableListOf<String>()
    val sizes = mutableMapOf<String, Int>()
}

fun box(): String {
    val recorder = Recorder()
    assertEquals(emptyList(), recorder.calls, "no calls yet")
    assertEquals(recorder.calls, emptyList())
    assertEquals(listOf(), recorder.calls)
    assertEquals(emptyMap(), recorder.sizes)
    recorder.calls += "a"
    assertEquals(listOf("a"), recorder.calls)
    return "OK"
}
"#;

#[test]
fn a_result_only_producer_witnesses_the_only_input_type() {
    let test_jar = common::kotlin_test_jar()
        .expect("no kotlin-test jar found; provision the reference toolchain (`just` fetches it)");
    common::assert_classes_identical_to_kotlinc_against(
        "OnlyInputTypeProducer",
        SOURCE,
        &["OnlyInputTypeProducerKt", "Recorder"],
        std::slice::from_ref(&test_jar),
    );
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            SOURCE,
            "OnlyInputTypeProducer",
            &[common::stdlib_jar(), test_jar, jdk.clone()],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}
