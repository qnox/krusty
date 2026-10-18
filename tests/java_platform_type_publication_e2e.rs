//! Inferred declarations retain a Java generic result's complete flexible interval and checked
//! `EnhancedNullability` argument marks in Kotlin metadata.

use super::common;

#[test]
fn inferred_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "Published",
        "fun <X> f(x: X) = java.util.Collections.singletonList(x)\n\
         fun <X> assignedReturn(x: X): List<X> {\n\
         \x20   var value = java.util.Collections.singletonList(x)\n\
         \x20   value = listOf(x)\n\
         \x20   return value\n\
         }\n\
         fun inheritedMember(xs: java.util.ArrayList<String>) = xs.get(0)\n\
         fun inheritedProperty(m: java.util.HashMap<String, String>) = m.keys\n\
         class Scope {\n\
         \x20   fun <T> T.forward(): T = this\n\
         \x20   val <T> T.forwardedProperty: T get() = this\n\
         \x20   fun forwarded(x: String) = java.util.Collections.singletonList(x).forward()\n\
         \x20   fun propertyForwarded(x: String) =\n\
         \x20       java.util.Collections.singletonList(x).forwardedProperty\n\
         }\n\
         class DispatchScope<T>(private val value: T) {\n\
         \x20   fun String.forwardedFunction(): T = value\n\
         \x20   val String.forwardedProperty: T get() = value\n\
         \x20   fun readFunction() = \"\".forwardedFunction()\n\
         \x20   fun readProperty() = \"\".forwardedProperty\n\
         }\n\
         fun dispatchFunctionForwarded(x: String) =\n\
         \x20   DispatchScope(java.util.Collections.singletonList(x)).readFunction()\n\
         fun dispatchPropertyForwarded(x: String) =\n\
         \x20   DispatchScope(java.util.Collections.singletonList(x)).readProperty()\n\
         val one = java.util.Collections.singletonList(\"a\")\n\
         val two get() = java.util.Collections.singletonList(\"b\")\n\
         fun g() = java.util.Collections.emptyList<String>()\n",
        &["PublishedKt", "DispatchScope", "Scope"],
    );
}

#[test]
fn inferred_member_expression_getter_publishes_platform_type_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedMemberGetter",
        "class MemberGetter {\n\
         \x20   val values get() = java.util.Collections.singletonList(\"member\")\n\
         }\n",
        &["MemberGetter"],
    );
}

#[test]
fn explicit_member_getter_alias_does_not_publish_body_flexibility() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedExplicitGetterAlias",
        "typealias Strings = List<String>\n\
         class ExplicitGetterAlias {\n\
         \x20   val values get(): Strings = java.util.Collections.singletonList(\"member\")\n\
         }\n",
        &["ExplicitGetterAlias", "PublishedExplicitGetterAliasKt"],
    );
}

#[test]
fn inferred_member_extension_getter_publishes_platform_type_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedMemberExtensionGetter",
        "class MemberExtensionGetter {\n\
         \x20   val String.values get() = java.util.Collections.singletonList(this)\n\
         }\n",
        &["MemberExtensionGetter"],
    );
}

#[test]
fn bare_member_extension_getter_forwards_platform_type_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedBareMemberExtensionGetter",
        "class BareMemberExtensionGetter {\n\
         \x20   val String.direct get() = java.util.Collections.singletonList(this)\n\
         \x20   val String.forwarded get() = direct\n\
         }\n",
        &["BareMemberExtensionGetter"],
    );
}

#[test]
fn forwarded_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedForwarded",
        "fun <T> id(x: T): T = x\n\
         fun <X> direct(x: X) = java.util.Collections.singletonList(x)\n\
         fun <X> forwarded(x: X) = id(id(java.util.Collections.singletonList(x)))\n",
        &["PublishedForwardedKt"],
    );
}

#[test]
fn control_flow_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedFlow",
        "fun blocked(flag: Boolean, x: String) = if (flag) {\n\
         \x20   val value = java.util.Collections.singletonList(x)\n\
         \x20   value\n\
         } else java.util.Collections.singletonList(x)\n\
         fun branched(flag: Boolean, x: String) = if (flag)\n\
         \x20   java.util.Collections.singletonList(x)\n\
         else java.util.Collections.singletonList(x)\n\
         fun elvis(x: String) = java.util.Collections.singletonList(x)\n\
         \x20   ?: java.util.Collections.singletonList(x)\n\
         fun selected(n: Int, x: String) = when (n) {\n\
         \x20   0 -> java.util.Collections.singletonList(x)\n\
         \x20   else -> java.util.Collections.singletonList(x)\n\
         }\n\
         fun recovered(x: String) = try {\n\
         \x20   java.util.Collections.singletonList(x)\n\
         } catch (e: Exception) {\n\
         \x20   java.util.Collections.singletonList(x)\n\
         }\n\
         fun reassigned(flag: Boolean, x: String) = if (flag) {\n\
         \x20   var value = java.util.Collections.singletonList(x)\n\
         \x20   value = listOf(x)\n\
         \x20   value\n\
         } else java.util.Collections.singletonList(x)\n",
        &["PublishedFlowKt"],
    );
}

#[test]
fn reassigned_java_generic_result_publishes_platform_type_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedReassigned",
        "fun reassigned(flag: Boolean, x: String) = if (flag) {\n\
         \x20   var value = java.util.Collections.singletonList(x)\n\
         \x20   value = listOf(x)\n\
         \x20   value\n\
         } else java.util.Collections.singletonList(x)\n",
        &["PublishedReassignedKt"],
    );
}

#[test]
fn immutable_local_java_generic_result_publishes_platform_type_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedImmutableLocal",
        "fun blocked(flag: Boolean, x: String) = if (flag) {\n\
         \x20   val value = java.util.Collections.singletonList(x)\n\
         \x20   value\n\
         } else java.util.Collections.singletonList(x)\n",
        &["PublishedImmutableLocalKt"],
    );
}

#[test]
fn untouched_mutable_local_forwards_enhanced_java_arguments_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedEnhancedMutableLocal",
        "fun forwarded(map: java.util.HashMap<String, Int>) = run {\n\
         \x20   var value = map.keys\n\
         \x20   value\n\
         }\n",
        &["PublishedEnhancedMutableLocalKt"],
    );
}

#[test]
fn reassigned_mutable_local_forwards_common_enhanced_java_arguments_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedReassignedEnhancedMutableLocal",
        "fun forwarded(map: java.util.HashMap<String, Int>) = run {\n\
         \x20   var value = map.keys\n\
         \x20   value = java.util.HashMap<String, Int>().keys\n\
         \x20   value\n\
         }\n",
        &["PublishedReassignedEnhancedMutableLocalKt"],
    );
}

#[test]
fn reassigned_mutable_local_drops_unshared_enhanced_java_arguments_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedClearedEnhancedMutableLocal",
        "fun forwarded(map: java.util.HashMap<String, Int>) = run {\n\
         \x20   var value = map.keys\n\
         \x20   value = mutableSetOf<String>()\n\
         \x20   value\n\
         }\n",
        &["PublishedClearedEnhancedMutableLocalKt"],
    );
}

#[test]
fn mutable_local_run_results_are_accepted_like_kotlinc_at_source_level() {
    common::expect_box_same_as_kotlinc(
        "fun untouched(map: java.util.HashMap<String, Int>): Set<String> = run {\n\
         \x20   var value = map.keys\n\
         \x20   value\n\
         }\n\
         fun commonlyReassigned(map: java.util.HashMap<String, Int>): Set<String> = run {\n\
         \x20   var value = map.keys\n\
         \x20   value = java.util.HashMap<String, Int>().keys\n\
         \x20   value\n\
         }\n\
         fun cleared(map: java.util.HashMap<String, Int>): Set<String> = run {\n\
         \x20   var value = map.keys\n\
         \x20   value = mutableSetOf<String>()\n\
         \x20   value\n\
         }\n\
         fun box(): String {\n\
         \x20   val map = java.util.HashMap<String, Int>()\n\
         \x20   map[\"OK\"] = 1\n\
         \x20   if (untouched(map).single() != \"OK\") return \"untouched\"\n\
         \x20   if (commonlyReassigned(map).isNotEmpty()) return \"reassigned\"\n\
         \x20   if (cleared(map).isNotEmpty()) return \"cleared\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "MutableLocalRunResults",
    );
}

#[test]
fn nested_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedNested",
        "class Box<T>(val value: T)\n\
         fun boxed(x: String) = Box(java.util.Collections.singletonList(x))\n\
         class PairBox<A, B>(val first: A, val second: B)\n\
         fun namedBox(x: String) = PairBox(second = 1,\n\
         \x20   first = java.util.Collections.singletonList(x))\n\
         class Carrier<T>(private val value: T) {\n\
         \x20   fun take(): T = value\n\
         \x20   val property: T get() = value\n\
         }\n\
         fun memberForwarded(x: String) =\n\
         \x20   Carrier(java.util.Collections.singletonList(x)).take()\n\
         fun propertyForwarded(x: String) =\n\
         \x20   Carrier(java.util.Collections.singletonList(x)).property\n",
        &["PublishedNestedKt", "Box", "Carrier", "PairBox"],
    );
}

#[test]
fn constructor_nested_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedConstructorNested",
        "class ConstructorBox<T>(val value: T)\n\
         fun boxed(x: String) = ConstructorBox(java.util.Collections.singletonList(x))\n\
         class ConstructorPairBox<A, B>(val first: A, val second: B)\n\
         fun namedBox(x: String) = ConstructorPairBox(second = 1,\n\
         \x20   first = java.util.Collections.singletonList(x))\n",
        &[
            "PublishedConstructorNestedKt",
            "ConstructorBox",
            "ConstructorPairBox",
        ],
    );
}

#[test]
fn member_nested_java_generic_results_publish_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedMemberNested",
        "class MemberCarrier<T>(private val value: T) {\n\
         \x20   fun take(): T = value\n\
         \x20   val property: T get() = value\n\
         }\n\
         fun memberForwarded(x: String) =\n\
         \x20   MemberCarrier(java.util.Collections.singletonList(x)).take()\n\
         fun propertyForwarded(x: String) =\n\
         \x20   MemberCarrier(java.util.Collections.singletonList(x)).property\n",
        &["PublishedMemberNestedKt", "MemberCarrier"],
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

#[test]
fn inferred_java_function_result_publishes_nested_platform_types_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "PublishedFunction",
        "fun published(block: (String) -> String) =\n\
         \x20   java.util.Collections.singletonList(block).get(0)\n",
        &["PublishedFunctionKt"],
    );
}
