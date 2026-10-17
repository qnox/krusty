//! Inferred declarations retain a Java generic result's complete flexible interval and checked
//! `EnhancedNullability` argument marks in Kotlin metadata.

use super::common;

#[test]
fn inferred_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "Published",
        "fun <X> f(x: X) = java.util.Collections.singletonList(x)\n\
         val one = java.util.Collections.singletonList(\"a\")\n\
         fun g() = java.util.Collections.emptyList<String>()\n",
        &["PublishedKt"],
    );
}

#[test]
fn inferred_java_array_result_publishes_its_out_projected_upper_bound_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedArray",
        "fun p(x: String) = java.util.Collections.singletonList(x)\n\
         fun a(xs: java.util.ArrayList<String>) = xs.toArray(arrayOf(\"z\"))\n",
        &["PublishedArrayKt"],
    );
}

#[test]
fn inferred_enhanced_java_results_publish_enhanced_nullability_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "Enhanced",
        "fun h(m: java.util.HashMap<String, Int>) = m.keys\n\
         val k = java.util.HashMap<String, Int>().keys\n\
         fun e(m: java.util.HashMap<String, Int>) = m.entries\n\
         class Holder(m: java.util.HashMap<String, Int>) { val keys = m.keys }\n",
        &["EnhancedKt", "Holder"],
    );
}
