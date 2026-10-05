//! kotlinc accumulates an annotation implementation's `hashCode` in a temporary named `result`: the
//! first member's term is stored, and each later term is added to a reload and stored again before the
//! final reload returns it. The temporary has a `LocalVariableTable` row from its first store to the
//! return, so the stores and loads stay in the method. A single-member annotation returns its one
//! term directly and has no `result`.
use super::common;

const SOURCE: &str = "package store\n\
                      \n\
                      annotation class Single(val name: String)\n\
                      \n\
                      annotation class Couple(val name: String, val count: Int)\n\
                      \n\
                      annotation class Many(val single: Single, val flag: Boolean, val size: Long)\n\
                      \n\
                      fun single(): Any = Single(\"a\")\n\
                      \n\
                      fun couple(): Any = Couple(\"a\", 1)\n\
                      \n\
                      fun many(): Any = Many(Single(\"b\"), true, 2L)\n";

#[test]
fn annotation_hash_code_result_is_byte_identical_to_kotlinc() {
    for class in [
        "store/AnnotationHashResultKt$annotationImpl$store_Single$0",
        "store/AnnotationHashResultKt$annotationImpl$store_Couple$0",
        "store/AnnotationHashResultKt$annotationImpl$store_Many$0",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "AnnotationHashResult",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
