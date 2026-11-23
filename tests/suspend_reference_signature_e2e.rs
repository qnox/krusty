//! A suspend function reference's class implements `Function1<Continuation<-R>, Object>`; the
//! continuation's `in` projection stays written over `Any`, as kotlinc writes it.

use super::common;

const SOURCE: &str = r#"suspend fun answer(): Any = "a"

suspend fun text(): String = "t"

fun references(): List<Any> = listOf(::answer, ::text)
"#;

#[test]
fn a_suspend_reference_class_writes_its_continuation_projection_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendReferenceSignature",
        SOURCE,
        &[
            "SuspendReferenceSignatureKt$references$1",
            "SuspendReferenceSignatureKt$references$2",
        ],
    );
}
