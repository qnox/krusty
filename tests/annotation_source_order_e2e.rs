//! kotlinc's `AnnotationCodegen` visits a declaration's annotations in source order and interns
//! each as it goes, whichever attribute (`RuntimeVisibleAnnotations` or
//! `RuntimeInvisibleAnnotations`) holds it. A binary-retention annotation written before a runtime
//! one is therefore interned first.
use super::common;

const SOURCE: &str = "package order\n\
                      \n\
                      @Retention(AnnotationRetention.BINARY)\n\
                      annotation class Internal\n\
                      \n\
                      @Internal\n\
                      @Deprecated(\"use another\")\n\
                      class Holder {\n\
                      \x20   @Internal\n\
                      \x20   @Deprecated(\"use another\")\n\
                      \x20   fun member() = 1\n\
                      }\n\
                      \n\
                      @Deprecated(\"use another\")\n\
                      @Internal\n\
                      class Reversed\n";

#[test]
fn annotations_intern_in_source_order_like_kotlinc() {
    for class in ["order/Holder", "order/Reversed"] {
        common::byte_diff_against_kotlinc_cp(
            "AnnotationSourceOrder",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
