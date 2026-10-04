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
    let expected: &[(KotlinVersion, &[&str])] = &[
        (KotlinVersion::V2_4_0, LEDGER),
        (KotlinVersion::V2_4_10, LEDGER),
        (KotlinVersion::V2_4_20, LEDGER),
    ];
    let target = krusty::kotlin_version::target();
    let (_, ledger) = expected
        .iter()
        .find(|(version, _)| *version == target)
        .unwrap_or_else(|| panic!("no expected ledger for kotlinc {target}"));
    let sources = [("Wrong.kt", WRONG_TARGETS)];
    assert_eq!(common::reference_error_ledger(&sources, &[]), *ledger);
    common::assert_errors_match_kotlinc(&sources, &[]);
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
