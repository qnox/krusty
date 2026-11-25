//! An inner class is named in a generic `Signature` with `.` only below the first classifier of
//! its outer chain that has type arguments. An outer chain without any is part of its binary name.

use super::common;

const SOURCE: &str = r#"class Plain {
    inner class Inner

    inner class Generic<Y>
}

class Open<T> {
    inner class Inner

    inner class Deep<U> {
        inner class Leaf<V>
    }
}

fun reference() = Plain::Inner

fun plain(): List<Plain.Inner> = emptyList()

fun generic(): List<Plain.Generic<String>> = emptyList()

fun open(): List<Open<String>.Inner> = emptyList()

fun deep(): List<Open<String>.Deep<Int>.Leaf<Long>> = emptyList()
"#;

#[test]
fn inner_classes_are_named_in_signatures_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "InnerClassSignatureName",
        SOURCE,
        &[
            "InnerClassSignatureNameKt",
            "Plain",
            "InnerClassSignatureNameKt$reference$1",
        ],
    );
}
