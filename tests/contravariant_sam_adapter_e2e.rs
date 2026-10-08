//! A contravariantly projected SAM keeps Kotlin source types in checked IR while the JVM adapter
//! receives the declaration's erased slot.

use super::common;

const JAVA: &str = r#"package neutral;
public interface Sink<T> {
    void accept(T value);
}
"#;

const SOURCE: &str = r#"package sample

import neutral.Sink

fun takeExact(sink: Sink<String?>) {}
fun takeProjected(sink: Sink<in String?>) {}

fun exact(action: (String?) -> Unit) {
    takeExact { action(it) }
}

fun projectedLiteral(action: (String?) -> Unit) {
    takeProjected { action(it) }
}

fun projectedValue(action: (String?) -> Unit) {
    takeProjected(action)
}

fun referenced(value: String?) {}

fun projectedReference() {
    takeProjected(::referenced)
}
"#;

#[test]
fn a_contravariant_sam_adapter_is_byte_identical_to_kotlinc() {
    let java = [("neutral/Sink.java".to_string(), JAVA.to_string())];
    let (library, _) =
        common::javac_compile(&java, &[]).expect("javac builds the repository-owned SAM");

    common::assert_classes_identical_to_kotlinc_against(
        "ProjectedSam",
        SOURCE,
        &[
            "sample/ProjectedSamKt",
            "sample/ProjectedSamKt$projectedReference$1",
        ],
        &[library],
    );
}
