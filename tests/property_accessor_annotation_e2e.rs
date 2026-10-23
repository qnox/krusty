//! Annotations written on a property ACCESSOR (`@A get()`, `@A set(v)`) and on a setter's value
//! parameter (`set(@A v)`). kotlinc resolves them like any declaration annotation, checks them
//! against the `getter`, `setter` and `value parameter` targets, writes them on the accessor
//! methods by retention, and records them in the property's `@Metadata`.

use super::common;

/// Both compilers build `sources` as one module and every class they write is byte-identical.
fn assert_module_matches_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn constructor_property_annotations_match_kotlinc_in_both_source_orders() {
    const POLICY: &str = r#"
import kotlin.reflect.KClass
@Target(AnnotationTarget.PROPERTY)
annotation class PropertyMark(val type: KClass<*>)
@Target(AnnotationTarget.PROPERTY, AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY_GETTER)
annotation class Other(val text: String)
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class ParamOnly(val text: String)
class Token
"#;
    const USE: &str = r#"
class Subject(
    @PropertyMark(Token::class) @param:Other("parameter") @get:Other("getter") val first: Int,
    @ParamOnly("default parameter") val second: Int,
    @property:Other("property") val third: Int
)
"#;
    for sources in [
        [("Subject.kt", USE), ("Marks.kt", POLICY)],
        [("Marks.kt", POLICY), ("Subject.kt", USE)],
    ] {
        assert_module_matches_kotlinc(&sources);
    }
}

#[test]
fn constructor_parameter_annotation_values_are_seeded_by_retention_before_the_body() {
    assert_module_matches_kotlinc(&[(
        "ConstructorAnnotations.kt",
        r#"
import kotlin.reflect.KClass
annotation class Nested(val text: String)
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class Visible(val text: String, val nested: Nested, val types: Array<KClass<*>>)
@Target(AnnotationTarget.VALUE_PARAMETER)
@Retention(AnnotationRetention.BINARY)
annotation class Invisible(val values: IntArray, val type: KClass<*>)
class Token
class Subject(
    @Invisible([3, 5], Token::class) @Visible("first", Nested("nested"), [Token::class]) val first: String,
    @Visible("second", Nested("other"), [String::class]) @Invisible([7], String::class) val second: String?
)
"#,
    )]);
}

#[test]
fn constructor_parameter_property_default_targets_match_kotlinc() {
    let policy = (
        "DefaultTargetPolicies.kt",
        r#"package defaults
@Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY, AnnotationTarget.FIELD,
        AnnotationTarget.PROPERTY_GETTER)
annotation class PropertyMark(val text: String)
@Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.FIELD)
annotation class FieldMark(val text: String)
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class ParameterMark(val text: String)
"#,
    );
    let usage = (
        "DefaultTargetSubject.kt",
        r#"package defaults
class Subject(
    @PropertyMark("both") val both: Int,
    @FieldMark("parameter and field") val parameterAndField: Int,
    @ParameterMark("parameter only") val parameterOnly: Int,
    @param:PropertyMark("explicit parameter") val explicitParameter: Int,
    @property:PropertyMark("explicit property") val explicitProperty: Int,
    @field:PropertyMark("explicit field") val explicitField: Int,
    @get:PropertyMark("explicit getter") val explicitGetter: Int,
    @PropertyMark("plain parameter") plain: Int
)
"#,
    );
    assert_module_matches_kotlinc(&[policy, usage]);
    assert_module_matches_kotlinc(&[usage, policy]);
}

#[test]
fn configured_default_targets_in_older_and_current_language_match_kotlinc() {
    let policies = r#"package configured
@Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.PROPERTY, AnnotationTarget.FIELD)
annotation class Mark(val text: String)
"#;
    let source = r#"package configured
class Subject(
    @Mark("default") val defaulted: Int,
    @param:Mark("parameter") val parameterOnly: Int,
    @property:Mark("property") val propertyOnly: Int,
    @field:Mark("field") val fieldOnly: Int
)
"#;
    for language in [
        krusty::language_version::LanguageVersion::V2_3,
        krusty::language_version::LanguageVersion::V2_4,
    ] {
        let settings = krusty::language_settings::LanguageSettings::new(language, None, &[])
            .expect("valid language settings");
        let classes = common::classes_against_kotlinc_lib_language_settings(
            "ConfiguredDefaultTargets",
            &[("Policies.kt", policies)],
            source,
            &settings,
        )
        .expect("reference compiler is provisioned");
        assert_eq!(classes.differences(), Vec::<String>::new(), "{language:?}");
    }
}

#[test]
fn fieldless_annotation_properties_do_not_gain_a_default_field_application() {
    assert_module_matches_kotlinc(&[(
        "FieldlessDefaultTargets.kt",
        r#"package fieldless
@Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.FIELD)
annotation class Mark(val text: String)
annotation class Subject(@Mark("parameter only") val value: Int)
"#,
    )]);
}

const ANNOTATIONS: &str = "package accessors\n\
    \n\
    @Retention(AnnotationRetention.RUNTIME)\n\
    annotation class Visible(val tag: String)\n\
    \n\
    @Retention(AnnotationRetention.BINARY)\n\
    annotation class Invisible\n\
    \n\
    @Target(AnnotationTarget.PROPERTY_GETTER, AnnotationTarget.PROPERTY_SETTER)\n\
    annotation class AccessorOnly\n\
    \n\
    @Retention(AnnotationRetention.SOURCE)\n\
    annotation class SourceOnly\n";

const USE_SITE_ANNOTATIONS: &str = "package sites\n\
    \n\
    annotation class Visible\n\
    \n\
    @Retention(AnnotationRetention.BINARY)\n\
    annotation class Invisible\n\
    \n\
    annotation class Tagged(val tag: String)\n\
    \n\
    @Target(AnnotationTarget.FIELD)\n\
    annotation class FieldOnly\n";

#[test]
fn use_site_targets_on_class_properties_match_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Annotations.kt", USE_SITE_ANNOTATIONS),
        (
            "Holder.kt",
            "package sites\n\
             \n\
             class Holder(\n\
             \x20   @field:Visible @param:Invisible val first: Int,\n\
             \x20   @get:Tagged(\"first\") @set:Invisible @setparam:Visible\n\
             \x20   @property:Visible @field:Invisible var second: String,\n\
             \x20   @param:Visible plain: Int,\n\
             ) {\n\
             \x20   @field:Visible\n\
             \x20   val a: Int = plain\n\
             \n\
             \x20   @field:Tagged(\"b\") @property:Invisible @FieldOnly\n\
             \x20   val b: String = \"b\"\n\
             \n\
             \x20   @get:Visible @set:Invisible @setparam:Tagged(\"c\")\n\
             \x20   var c: String = \"c\"\n\
             \n\
             \x20   @get:Invisible\n\
             \x20   val d: Int get() = 4\n\
             }\n",
        ),
    ]);
}

/// A property-prefix use-site annotation surrounds an accessor's own annotations in the order
/// kotlinc visits them. A generated setter also retains its value-parameter application.
#[test]
fn use_site_targets_beside_declared_accessors_match_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Annotations.kt", USE_SITE_ANNOTATIONS),
        (
            "Accessors.kt",
            "package sites\n\
             \n\
             class Accessors {\n\
             \x20   @get:Visible\n\
             \x20   val x: Int = 1\n\
             \x20       @Invisible get() = field\n\
             \n\
             \x20   @set:Visible @setparam:Invisible\n\
             \x20   var y: Int = 1\n\
             \x20       @Invisible set(v) { field = v }\n\
             \n\
             \x20   @setparam:Visible\n\
             \x20   var z: String = \"\"\n\
             \n\
             \x20   @get:Visible\n\
             \x20   val bodylessGetter: Int = 1\n\
             \x20       @Invisible get\n\
             \n\
             \x20   @set:Visible @setparam:Tagged(\"ignored\")\n\
             \x20   var bodylessSetter: Int = 1\n\
             \x20       @Invisible set\n\
             }\n",
        ),
    ]);
}

#[test]
fn wrong_use_site_targets_are_retained_for_exact_diagnostics() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "WrongUseSites.kt",
            "@Target(AnnotationTarget.FIELD) annotation class F\n\
             @Target(AnnotationTarget.VALUE_PARAMETER) annotation class ParameterOnly\n\
             @Target(AnnotationTarget.PROPERTY_SETTER) annotation class SetterOnly\n\
             class Holder(\n\
             \x20   @get:F val constructorValue: Int,\n\
             ) {\n\
             \x20   @get:F val value: Int = 1\n\
             \x20   @set:F @setparam:F var mutable: Int = 1\n\
             \x20   @param:ParameterOnly val notAParameter: Int = 1\n\
             \x20   @set:SetterOnly val immutable: Int = 1\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn annotations_on_member_accessors_match_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Annotations.kt", ANNOTATIONS),
        (
            "Holder.kt",
            "package accessors\n\
             \n\
             class Holder {\n\
             \x20   var count: Int = 0\n\
             \x20       @Visible(\"get\") @Invisible get() = field\n\
             \x20       @Visible(\"set\") @AccessorOnly set(value) {\n\
             \x20           field = value\n\
             \x20       }\n\
             \n\
             \x20   val name: String\n\
             \x20       @SourceOnly @Invisible get() = \"name\"\n\
             \n\
             \x20   var plain: String = \"\" @Invisible get @Invisible set\n\
             \n\
             \x20   var checked: Int = 0\n\
             \x20       set(@Invisible @Visible(\"value\") value) {\n\
             \x20           field = value\n\
             \x20       }\n\
             }\n",
        ),
    ]);
}

#[test]
fn same_named_member_and_extension_properties_keep_distinct_annotation_markers() {
    assert_module_matches_kotlinc(&[(
        "OverloadedMarkers.kt",
        "package accessors\n\
         \n\
         @Target(AnnotationTarget.PROPERTY)\n\
         annotation class Marker(val value: String)\n\
         \n\
         class OverloadedMarkers {\n\
         \x20   @Marker(\"member\") val item: Int = 1\n\
         \x20   @Marker(\"extension\") val String.item: Int get() = length\n\
         }\n",
    )]);
}

#[test]
fn a_member_extension_marker_clash_renders_its_receiver_parameter() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "ExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   @Marker val String.item: Int get() = length\n\
             \x20   fun `getItem$annotations`(receiver: String) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_marker_clash_escapes_its_generic_receiver_parameter() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "GenericExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<`T value`> {\n\
             \x20   @Marker val `T value`.item: Int get() = 1\n\
             \x20   fun `getItem$annotations`(receiver: `T value`) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_marker_clash_lists_a_different_source_shape_first() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "GenericExtensionMarkerOrder.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<A> {\n\
             \x20   @Marker val A.item: Int get() = 1\n\
             \x20   fun `getItem$annotations`(receiver: Any?) {}\n\
             \x20   fun `getItem$annotations`(receiver: A) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn marker_clash_qualifies_same_named_classifier_shapes_across_the_group() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "QualifiedExtensionMarkerClash.kt",
            "class A { class Foo }\n\
             class B { class Foo }\n\
             @Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   @Marker val List<A.Foo>.item: Int get() = size\n\
             \x20   fun `getItem$annotations`(receiver: List<B.Foo>) {}\n\
             \x20   fun `getItem$annotations`(receiver: List<A.Foo>) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_marker_clash_replaces_only_nested_type_parameters() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "NestedGenericExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<T> {\n\
             \x20   @Marker val List<T>.item: Int get() = size\n\
             \x20   fun `getItem$annotations`(receiver: List<T>) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_marker_clash_star_projects_a_generic_bound() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "BoundedGenericExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<T : List<String>> {\n\
             \x20   @Marker val T.item: Int get() = size\n\
             \x20   fun `getItem$annotations`(receiver: T) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_marker_bound_and_star_projection_with_the_same_diagnostic_shape_have_stable_order() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "SameDiagnosticShapeExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<T : List<String>> {\n\
             \x20   @Marker val T.item: Int get() = size\n\
             \x20   fun `getItem$annotations`(receiver: List<*>) {}\n\
             \x20   fun `getItem$annotations`(receiver: T) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn independent_marker_clash_groups_follow_property_source_order() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "IndependentExtensionMarkerClashes.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<T> {\n\
             \x20   @Marker val T.zed: Int get() = 1\n\
             \x20   fun `getZed$annotations`(receiver: T) {}\n\
             \x20   @Marker val T.alpha: Int get() = 1\n\
             \x20   fun `getAlpha$annotations`(receiver: T) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_marker_clash_erases_a_function_bound() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FunctionBoundExtensionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C<T : (String) -> Int> {\n\
             \x20   @Marker val T.item: Int get() = 1\n\
             \x20   fun `getItem$annotations`(receiver: T) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_member_extension_function_marker_clash_renders_its_receiver() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "ExtensionFunctionMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   @Marker val String.item: Int get() = length\n\
             \x20   fun String.`getItem$annotations`() {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_renamed_property_accessor_clashes_with_an_extension_marker() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "AccessorMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   @Marker val String.item: Int get() = length\n\
             \x20   var other: String = \"\"\n\
             \x20       @JvmName(\"getItem\\$annotations\") set(value) { field = value }\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_renamed_member_extension_accessor_clashes_with_a_context_extension_marker() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "MemberExtensionAccessorMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   context(_: Int)\n\
             \x20   @Marker val String.item: Int get() = length\n\
             \x20   var Int.other: String\n\
             \x20       get() = \"\"\n\
             \x20       @JvmName(\"getItem\\$annotations\") set(value) {}\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_renamed_member_extension_accessor_clashes_with_a_generated_data_method() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "GeneratedDataMethodClash.kt",
            "data class D(val x: Int) {\n\
             \x20   val Int.foo: D\n\
             \x20       @JvmName(\"copy\") get() = this@D\n\
             }\n",
        )],
        &[],
    );
}

#[test]
fn a_renamed_companion_block_accessor_clashes_with_a_context_property_marker() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "CompanionBlockAccessorMarkerClash.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class Marker\n\
             class C {\n\
             \x20   context(_: Int)\n\
             \x20   @Marker val item: Int get() = 0\n\
             \x20   companion {\n\
             \x20       var other: Int = 0\n\
             \x20           @JvmName(\"getItem\\$annotations\") set(value) { field = value }\n\
             \x20   }\n\
             }\n",
        )],
        &["-XXLanguage:+CompanionBlocksAndExtensions".to_string()],
    );
}

#[test]
fn annotations_on_top_level_accessors_match_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Annotations.kt", ANNOTATIONS),
        (
            "Top.kt",
            "package accessors\n\
             \n\
             var top: Int = 1\n\
             \x20   @Visible(\"top\") get() = field\n\
             \x20   set(@Invisible value) {\n\
             \x20       field = value\n\
             \x20   }\n\
             \n\
             val computed: Int\n\
             \x20   @AccessorOnly @Invisible get() = 2\n",
        ),
    ]);
}

#[test]
fn annotations_on_companion_accessors_match_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Annotations.kt", ANNOTATIONS),
        (
            "Registry.kt",
            "package accessors\n\
             \n\
             class Registry {\n\
             \x20   companion object {\n\
             \x20       var size: Int = 1\n\
             \x20           @Invisible get() = field\n\
             \x20           @Visible(\"size\") set(value) {\n\
             \x20               field = value\n\
             \x20           }\n\
             \x20   }\n\
             }\n",
        ),
    ]);
}

/// `@JvmName` on a declared accessor renames the accessor method, and `@Metadata`'s
/// `JvmPropertySignature` names the renamed method.
#[test]
fn a_jvm_name_on_a_declared_accessor_renames_it_like_kotlinc() {
    assert_module_matches_kotlinc(&[(
        "Renamed.kt",
        "package accessors\n\
         \n\
         class Box(var raw: Int) {\n\
         \x20   var n: Int\n\
         \x20       @JvmName(\"grab\") get() = raw\n\
         \x20       @JvmName(\"stash\") set(v) { raw = v }\n\
         }\n\
         \n\
         val Box.tag: String @JvmName(\"grabTag\") get() = \"T:\" + raw\n\
         \n\
         var counter: Int = 1\n\
         \x20   @JvmName(\"readCounter\") get() = field\n\
         \x20   @JvmName(\"writeCounter\") set(value) { field = value }\n",
    )]);
}

const WRONG_TARGETS: &str = "package accessors\n\
    \n\
    @Target(AnnotationTarget.FUNCTION)\n\
    annotation class FunctionOnly\n\
    \n\
    @Target(AnnotationTarget.PROPERTY)\n\
    annotation class PropertyOnly\n\
    \n\
    @Target(AnnotationTarget.VALUE_PARAMETER)\n\
    annotation class ParameterOnly\n\
    \n\
    @Target(AnnotationTarget.PROPERTY_GETTER)\n\
    annotation class GetterOnly\n\
    \n\
    @Target(AnnotationTarget.FUNCTION, AnnotationTarget.FIELD, AnnotationTarget.CLASS)\n\
    annotation class Several\n\
    \n\
    @Target(AnnotationTarget.FUNCTION, AnnotationTarget.FUNCTION)\n\
    annotation class Repeated\n\
    \n\
    @Target(AnnotationTarget.TYPE)\n\
    annotation class TypeOnly\n\
    \n\
    class Wrong {\n\
    \x20   var a: Int = 0\n\
    \x20       @FunctionOnly get\n\
    \x20       @PropertyOnly set\n\
    \x20   var b: Int = 0\n\
    \x20       @ParameterOnly get() = field\n\
    \x20       @GetterOnly set(@GetterOnly value) { field = value }\n\
    \x20   var c: Int = 0\n\
    \x20       @Several @Repeated get\n\
    \x20       @TypeOnly set\n\
    }\n\
    \n\
    var top: Int = 0\n\
    \x20   @FunctionOnly get() = field\n\
    \x20   set(@GetterOnly value) { field = value }\n";

/// kotlinc's WRONG_ANNOTATION_TARGET, at the annotation's `@`, naming the accessor's target and the
/// annotation's applicable targets in their declared order.
#[test]
fn an_annotation_on_a_wrong_accessor_target_is_rejected_like_kotlinc() {
    use krusty::kotlin_version::KotlinVersion;
    const LEDGER: &[&str] = &[
        "Wrong.kt:26:9: this annotation is not applicable to target 'getter'. Applicable targets: \
         function",
        "Wrong.kt:27:9: this annotation is not applicable to target 'setter'. Applicable targets: \
         property",
        "Wrong.kt:29:9: this annotation is not applicable to target 'getter'. Applicable targets: \
         value parameter",
        "Wrong.kt:30:9: this annotation is not applicable to target 'setter'. Applicable targets: \
         getter",
        "Wrong.kt:30:25: this annotation is not applicable to target 'value parameter'. \
         Applicable targets: getter",
        "Wrong.kt:32:9: this annotation is not applicable to target 'getter'. Applicable targets: \
         function, field, class",
        "Wrong.kt:32:18: this annotation is not applicable to target 'getter'. Applicable targets: \
         function",
        "Wrong.kt:33:9: this annotation is not applicable to target 'setter'. Applicable targets: \
         type usage",
        "Wrong.kt:37:5: this annotation is not applicable to target 'getter'. Applicable targets: \
         function",
        "Wrong.kt:38:9: this annotation is not applicable to target 'value parameter'. \
         Applicable targets: getter",
    ];
    let ledger: &[&str] = common::for_reference(&[(KotlinVersion::V2_4_0, LEDGER)]);
    let sources = [("Wrong.kt", WRONG_TARGETS)];
    assert_eq!(common::reference_error_ledger(&sources, &[]), ledger);
    common::assert_errors_match_kotlinc(&sources, &[]);
}

/// An explicit property use-site is a semantic target selection, not an emission hint. The
/// annotation class must allow that exact target before the checked application is published.
#[test]
fn an_explicit_field_site_requires_a_field_target() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class PropertyOnly\n\
             class C {\n\
             \x20   @field:PropertyOnly val value: Int = 1\n\
             }\n",
        )],
        &[],
    );
}

/// Target applicability is independent of argument typing. kotlinc reports the bad argument first
/// and the explicit-site error second; neither failure suppresses the other.
#[test]
fn an_explicit_field_site_is_checked_after_invalid_arguments() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.PROPERTY)\n\
             annotation class PropertyOnly(val number: Int)\n\
             class C {\n\
             \x20   @field:PropertyOnly(\"bad\") val value: Int = 1\n\
             }\n",
        )],
        &[],
    );
}

/// Allowing `FIELD` on the annotation class is not enough: an explicit `@field:` still requires
/// the annotated property declaration to own a backing field.
#[test]
fn an_explicit_field_site_rejects_a_computed_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FIELD)\n\
             annotation class FieldOnly\n\
             class C {\n\
             \x20   @field:FieldOnly val value: Int get() = 1\n\
             }\n",
        )],
        &[],
    );
}

/// Interface ownership makes a body-less property implicitly abstract; the absence of a written
/// `abstract` modifier does not give an explicit `@field:` application storage to target.
#[test]
fn an_explicit_field_site_rejects_an_implicit_abstract_interface_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FIELD)\n\
             annotation class FieldOnly\n\
             interface I {\n\
             \x20   @field:FieldOnly val value: Int\n\
             }\n",
        )],
        &[],
    );
}

/// An annotation-class constructor `val` declares an annotation element on an interface, not a
/// backing field that can receive an explicit `@field:` application.
#[test]
fn an_explicit_field_site_rejects_an_annotation_class_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FIELD)\n\
             annotation class FieldOnly\n\
             annotation class Token(@field:FieldOnly val value: Int)\n",
        )],
        &[],
    );
}

/// A field-only annotation selects the same unavailable field implicitly. The diagnostic still
/// names the annotation element's property host, but has no explicit use-site suffix.
#[test]
fn an_implicit_field_site_rejects_an_annotation_class_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FIELD)\n\
             annotation class FieldOnly\n\
             annotation class Token(@FieldOnly val value: Int)\n",
        )],
        &[],
    );
}

/// A primary-constructor property defaults to the value-parameter site before considering its
/// field, including when that constructor is the element list of an annotation class.
#[test]
fn an_annotation_class_property_defaults_to_its_constructor_parameter_site() {
    assert_module_matches_kotlinc(&[(
        "ConstructorPropertySite.kt",
        "@Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.VALUE_PARAMETER, AnnotationTarget.FIELD)\n\
         annotation class Mark\n\
         annotation class Token(@Mark val value: Int)\n",
    )]);
}

/// A use-site target is constrained by its declaration host even when the annotation class permits
/// both the declaration's ordinary target and `FIELD`.
#[test]
fn an_explicit_field_site_rejects_a_non_property_declaration() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.CLASS, AnnotationTarget.FIELD)\n\
             annotation class ClassOrField\n\
             @field:ClassOrField class C\n",
        )],
        &[],
    );
}

/// Suppression silences the invalid-site diagnostic, and kotlinc retains the application on the
/// actual class declaration despite the `@field:` prefix.
#[test]
fn a_suppressed_field_site_on_a_class_is_published_on_the_class() {
    assert_module_matches_kotlinc(&[(
        "FieldSite.kt",
        "@Retention(AnnotationRetention.RUNTIME)\n\
         @Target(AnnotationTarget.CLASS, AnnotationTarget.FIELD)\n\
         annotation class ClassOrField\n\
         @Suppress(\"WRONG_ANNOTATION_TARGET_WITH_USE_SITE_TARGET\")\n\
         @field:ClassOrField class C\n",
    )]);
}

/// A source member is not the synthetic carrier merely because its spelling matches the marker
/// name. The generated no-arg marker and the source declaration are a physical JVM clash.
#[test]
fn a_suppressed_property_marker_clashes_with_a_same_signature_source_member() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "MarkerClash.kt",
            "class C {\n\
             \x20   @Suppress(\"VOLATILE_ON_VALUE\")\n\
             \x20   @Volatile val value: Int = 1\n\
             \x20   fun `getValue$annotations`() {}\n\
             }\n",
        )],
        &[],
    );
}

/// A same-named source overload with a parameter remains separate from the generated `()V`
/// marker. Marker reuse is selected by compiler provenance and exact signature, not spelling.
#[test]
fn a_suppressed_property_marker_coexists_with_a_source_overload() {
    assert_module_matches_kotlinc(&[(
        "MarkerOverload.kt",
        "class C {\n\
         \x20   @Suppress(\"VOLATILE_ON_VALUE\")\n\
         \x20   @Volatile val value: Int = 1\n\
         \x20   fun `getValue$annotations`(number: Int) {}\n\
         }\n",
    )]);
}

/// A Kotlin `Unit` property getter returns the boxed `kotlin.Unit`; it does not use the JVM `void`
/// descriptor of an annotation marker. The two same-named methods therefore coexist.
#[test]
fn a_suppressed_property_marker_coexists_with_a_unit_property_getter() {
    assert_module_matches_kotlinc(&[(
        "MarkerProperty.kt",
        "class C {\n\
         \x20   @Suppress(\"VOLATILE_ON_VALUE\")\n\
         \x20   @Volatile val foo: Int = 1\n\
         \x20   val `foo$annotations`: Unit = Unit\n\
         }\n",
    )]);
}

/// Function declarations use the same host-aware rejection rather than treating `FIELD` in the
/// annotation class's own target set as permission for an invalid use-site prefix.
#[test]
fn an_explicit_field_site_rejects_a_function_declaration() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FUNCTION, AnnotationTarget.FIELD)\n\
             annotation class FunctionOrField\n\
             @field:FunctionOrField fun call() {}\n",
        )],
        &[],
    );
}

/// A field-only annotation defaults to the backing-field site, which is still invalid when the
/// property is computed and has no field to receive it.
#[test]
fn an_implicit_field_site_rejects_a_computed_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Target(AnnotationTarget.FIELD)\n\
             annotation class FieldOnly\n\
             class C {\n\
             \x20   @FieldOnly\n\
             \x20   var value: Int\n\
             \x20       get() = 1\n\
             \x20       set(value) {}\n\
             }\n",
        )],
        &[],
    );
}

/// An annotation retained on a transient local-declaration statement still rejects the property
/// use-site prefix against the local-variable host.
#[test]
fn an_explicit_field_site_rejects_a_local_declaration() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldSite.kt",
            "@Retention(AnnotationRetention.SOURCE)\n\
             @Target(AnnotationTarget.FIELD, AnnotationTarget.LOCAL_VARIABLE)\n\
             annotation class FieldOrLocal\n\
             fun call() { @field:FieldOrLocal val value = 1 }\n",
        )],
        &[],
    );
}

/// A Java `@Target` maps each `ElementType` to its Kotlin targets; the diagnostic lists their
/// union in Kotlin target order, then `expression`.
#[test]
fn a_java_annotation_on_a_wrong_accessor_target_is_rejected_like_kotlinc() {
    let java = [
        (
            "TypeAndField.java".into(),
            "package jt;\n\
             import java.lang.annotation.*;\n\
             @Target({ElementType.TYPE, ElementType.FIELD})\n\
             public @interface TypeAndField {}\n"
                .into(),
        ),
        (
            "FieldAndMethod.java".into(),
            "package jt;\n\
             import java.lang.annotation.*;\n\
             @Target({ElementType.FIELD, ElementType.METHOD})\n\
             public @interface FieldAndMethod {}\n"
                .into(),
        ),
    ];
    let (library, _) = common::javac_compile(&java, &[]).expect("javac compiles the annotations");
    let source = "import jt.*\n\
        \n\
        class Uses {\n\
        \x20   var a: Int = 0\n\
        \x20       @FieldAndMethod get\n\
        \x20       @TypeAndField set\n\
        \x20   var b: Int = 0\n\
        \x20       set(@FieldAndMethod value) { field = value }\n\
        }\n";
    let result =
        common::compiler_diagnostics(&[("Uses.kt", source)], &[library, common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "java accessor targets");
    let rendered = common::compiler_errors(&result.reference_stderr)
        .into_iter()
        .map(|error| {
            format!(
                "{}:{}:{}: {}",
                error.file, error.line, error.column, error.message
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        [
            "Uses.kt:6:9: this annotation is not applicable to target 'setter'. Applicable \
             targets: class, field, file, expression",
            "Uses.kt:8:13: this annotation is not applicable to target 'value parameter'. \
             Applicable targets: field, function, getter, setter, expression",
        ]
    );
}

/// `@Target`'s arguments are the enum entries the checker selected through the ordinary import
/// rules: an unqualified imported entry, and an aliased import whose alias spells another target.
const IMPORTED_TARGETS: &str = "package accessors\n\
    \n\
    import kotlin.annotation.AnnotationTarget.PROPERTY_GETTER\n\
    import kotlin.annotation.AnnotationTarget.PROPERTY_SETTER as CLASS\n\
    \n\
    @Target(PROPERTY_GETTER)\n\
    @Retention(AnnotationRetention.RUNTIME)\n\
    annotation class GetterOnly\n\
    \n\
    @Target(CLASS)\n\
    annotation class SetterOnly\n";

#[test]
fn an_imported_target_entry_is_applied_like_kotlinc() {
    assert_module_matches_kotlinc(&[
        ("Targets.kt", IMPORTED_TARGETS),
        (
            "Uses.kt",
            "package accessors\n\
             \n\
             class Holder {\n\
             \x20   val x: Int @GetterOnly get() = 1\n\
             \x20   var y: Int = 0\n\
             \x20       @GetterOnly get() = field\n\
             \x20       @SetterOnly set(value) {\n\
             \x20           field = value\n\
             \x20       }\n\
             }\n",
        ),
    ]);
}

/// The selected entry decides the target, not the spelling: the alias `CLASS` selects
/// `PROPERTY_SETTER`.
#[test]
fn an_imported_target_entry_is_checked_by_the_entry_it_selects_like_kotlinc() {
    use krusty::kotlin_version::KotlinVersion;
    const LEDGER: &[&str] = &[
        "Uses.kt:5:9: this annotation is not applicable to target 'setter'. Applicable targets: \
         getter",
        "Uses.kt:6:16: this annotation is not applicable to target 'getter'. Applicable targets: \
         setter",
    ];
    let ledger: &[&str] = common::for_reference(&[(KotlinVersion::V2_4_0, LEDGER)]);
    let sources = [
        ("Targets.kt", IMPORTED_TARGETS),
        (
            "Uses.kt",
            "package accessors\n\
             \n\
             class Holder {\n\
             \x20   var y: Int = 0\n\
             \x20       @GetterOnly set\n\
             \x20   val z: Int @SetterOnly get() = 1\n\
             }\n",
        ),
    ];
    assert_eq!(common::reference_error_ledger(&sources, &[]), ledger);
    common::assert_errors_match_kotlinc(&sources, &[]);
}

/// An entry of a repository-owned enum is an argument type mismatch, and the class gets no target
/// policy. kotlinc (2.4.0, 2.4.10 and 2.4.20 alike) still reports secondary WRONG_ANNOTATION_TARGET
/// diagnostics from the entry the invalid argument selected, by its declared name:
/// `MyTarget.PROPERTY_GETTER` lists `getter`, and an entry no target is named after lists nothing.
/// That recovery is diagnostic-only (see `frontend::tests::annotation_publication`).
#[test]
fn a_same_named_entry_of_another_enum_is_rejected_like_kotlinc() {
    use krusty::kotlin_version::KotlinVersion;
    const LEDGER: &[&str] = &[
        "Mismatch.kt:5:9: argument type mismatch: actual type is 'MyTarget', but \
         'AnnotationTarget' was expected.",
        "Mismatch.kt:8:9: argument type mismatch: actual type is 'MyTarget', but \
         'AnnotationTarget' was expected.",
        "Mismatch.kt:14:9: this annotation is not applicable to target 'setter'. Applicable \
         targets: getter",
        "Mismatch.kt:16:9: this annotation is not applicable to target 'getter'. Applicable \
         targets:",
    ];
    let ledger: &[&str] = common::for_reference(&[(KotlinVersion::V2_4_0, LEDGER)]);
    let sources = [(
        "Mismatch.kt",
        "package accessors\n\
         \n\
         enum class MyTarget { PROPERTY_GETTER, NOT_A_TARGET }\n\
         \n\
         @Target(MyTarget.PROPERTY_GETTER)\n\
         annotation class GetterOnly\n\
         \n\
         @Target(MyTarget.NOT_A_TARGET)\n\
         annotation class Nowhere\n\
         \n\
         class Holder {\n\
         \x20   var a: Int = 0\n\
         \x20       @GetterOnly get\n\
         \x20       @GetterOnly set\n\
         \x20   val b: Int\n\
         \x20       @Nowhere get() = 1\n\
         }\n",
    )];
    assert_eq!(common::reference_error_ledger(&sources, &[]), ledger);
    common::assert_errors_match_kotlinc(&sources, &[]);
}
