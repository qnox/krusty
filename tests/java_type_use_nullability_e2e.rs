//! A `TYPE_USE` nullability qualifier on a Java member's outermost type fixes that type's
//! nullability exactly as a declaration annotation does. A `@Nullable` return or field is `T?`, a
//! `@NonNull` parameter is `T` (an override taking `T?` overrides nothing), and a `@NonNull T` stays
//! `T & Any` under a nullable argument. The qualifier is read from a classfile's
//! `Runtime*TypeAnnotations` and from a Java source's annotation uses alike. Kotlin flattens a
//! qualifier on every classfile nested-class segment onto the member result. Its Java-source
//! frontend instead recognizes the outermost and innermost segments but not an intermediate one;
//! Krusty's provider boundaries preserve that observed policy. A non-static inner constructor also
//! aligns source parameter annotations after its enclosing-instance descriptor slot when its
//! classfile omits `MethodParameters`. Array component and nested-argument annotations must not
//! qualify the result; an annotation on the array dimension does qualify it.

use super::common;

fn annotation(name: &str, retention: &str) -> String {
    format!(
        "package org.checkerframework.checker.nullness.qual;\n\
         import java.lang.annotation.*;\n\
         @Target({{ElementType.TYPE_USE, ElementType.TYPE_PARAMETER}})\n\
         @Retention(RetentionPolicy.{retention})\n\
         public @interface {name} {{}}\n"
    )
}

const API: &str = "import org.checkerframework.checker.nullness.qual.NonNull;\n\
    import org.checkerframework.checker.nullness.qual.Nullable;\n\
    class Outer { public class Inner { public int value; } }\n\
    class Three { public class Middle { public class Inner { public int value; } } }\n\
    class ConstructorOuter {\n\
    \x20   public class Inner { public Inner(@NonNull String required, String flexible) {} }\n\
    }\n\
    public class Api<T extends @Nullable Object> {\n\
    \x20   public @Nullable String label;\n\
    \x20   private final T value;\n\
    \x20   public Api(T value) { this.value = value; }\n\
    \x20   public @Nullable String describe() { return null; }\n\
    \x20   public @NonNull T required() { return value; }\n\
    \x20   public void take(@NonNull String p0) {}\n\
    \x20   public @Nullable String[] nullableComponent;\n\
    \x20   public String @Nullable [] nullableArray;\n\
    \x20   public java.util.List<@Nullable String> nestedArgument;\n\
    \x20   public @Nullable Outer.Inner nullableOuterSegment;\n\
    \x20   public Outer.@Nullable Inner nullableInnerSegment;\n\
    \x20   public @Nullable Three.Middle.Inner nullableOuterThree;\n\
    \x20   public Three.@Nullable Middle.Inner nullableMiddleThree;\n\
    \x20   public Three.Middle.@Nullable Inner nullableInnerThree;\n\
    }\n";

const USE: &str = "fun describe(api: Api<String>): Int = api.describe().length\n\
    fun label(api: Api<String>): Int = api.label.length\n\
    fun component(api: Api<String>): Int = api.nullableComponent.size\n\
    fun array(api: Api<String>): Int = api.nullableArray.size\n\
    fun nested(api: Api<String>): Int = api.nestedArgument.size\n\
    class Lenient : Api<String>(\"x\") { override fun take(text: String?) {} }\n\
    fun required(api: Api<String?>): String = api.required()\n\
    fun reassign(api: Api<String?>): String {\n\
    \x20   var value = api.required()\n\
    \x20   value = null\n\
    \x20   return value\n\
    }\n\
    fun outerSegment(api: Api<String>): Int = api.nullableOuterSegment.value\n\
    fun innerSegment(api: Api<String>): Int = api.nullableInnerSegment.value\n\
    fun outerThree(api: Api<String>): Int = api.nullableOuterThree.value\n\
    fun middleThree(api: Api<String>): Int = api.nullableMiddleThree.value\n\
    fun innerThree(api: Api<String>): Int = api.nullableInnerThree.value\n\
    private fun construct(owner: ConstructorOuter) { owner.Inner(null, null) }\n";

fn annotation_sources(retention: &str) -> [(String, String); 2] {
    [
        ("NonNull.java".to_string(), annotation("NonNull", retention)),
        (
            "Nullable.java".to_string(),
            annotation("Nullable", retention),
        ),
    ]
}

fn assert_rejected_like_kotlinc(
    result: &common::CompilerDiagnosticResult,
    tag: &str,
    classfile: bool,
) {
    const KRUSTY_CLASSFILE_ERRORS: [&str; 12] = [
        "Use.kt:1:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:2:45: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:4:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Array<String!>?'.",
        "Use.kt:6:36: 'take' overrides nothing. Potential signatures for overriding:",
        "| fun take(p0: String): Unit",
        "Use.kt:10:13: null cannot be a value of a non-null type 'String'.",
        "Use.kt:13:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:14:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:15:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:16:65: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:17:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:18:62: null cannot be a value of a non-null type 'String'.",
    ];
    // Krusty's semantic array model stores the invariant lower bound while kotlinc renders the
    // flexible upper-bound projection. Classfiles flatten every pure INNER_TYPE path. Kotlinc's
    // Java-source provider omits the intermediate segment on line 16 while keeping the outermost
    // and innermost segments; both provider-specific complete ledgers are pinned below.
    const KOTLINC_CLASSFILE_ERRORS: [&str; 12] = [
        "Use.kt:1:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:2:45: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:4:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Array<(out) String!>?'.",
        "Use.kt:6:36: 'take' overrides nothing. Potential signatures for overriding:",
        "| fun take(p0: String): Unit",
        "Use.kt:10:13: null cannot be a value of a non-null type 'String'.",
        "Use.kt:13:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:14:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:15:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:16:65: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:17:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:18:62: null cannot be a value of a non-null type 'String'.",
    ];
    const KRUSTY_SOURCE_ERRORS: [&str; 11] = [
        "Use.kt:1:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:2:45: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:4:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Array<String!>?'.",
        "Use.kt:6:36: 'take' overrides nothing. Potential signatures for overriding:",
        "| fun take(p0: String): Unit",
        "Use.kt:10:13: null cannot be a value of a non-null type 'String'.",
        "Use.kt:13:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:14:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:15:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:17:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:18:62: null cannot be a value of a non-null type 'String'.",
    ];
    const KOTLINC_SOURCE_ERRORS: [&str; 11] = [
        "Use.kt:1:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:2:45: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'String?'.",
        "Use.kt:4:53: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Array<(out) String!>?'.",
        "Use.kt:6:36: 'take' overrides nothing. Potential signatures for overriding:",
        "| fun take(p0: String): Unit",
        "Use.kt:10:13: null cannot be a value of a non-null type 'String'.",
        "Use.kt:13:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:14:67: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Outer.Inner?'.",
        "Use.kt:15:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:17:63: only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'Three.Middle.Inner?'.",
        "Use.kt:18:62: null cannot be a value of a non-null type 'String'.",
    ];

    let (krusty_errors, kotlinc_errors): (&[&str], &[&str]) = if classfile {
        (&KRUSTY_CLASSFILE_ERRORS, &KOTLINC_CLASSFILE_ERRORS)
    } else {
        (&KRUSTY_SOURCE_ERRORS, &KOTLINC_SOURCE_ERRORS)
    };

    assert_eq!(result.krusty_code, 1, "{tag}: {}", result.krusty_stderr);
    assert_eq!(
        result.reference_code, 1,
        "{tag}: {}",
        result.reference_stderr
    );
    assert_eq!(result.krusty_stdout, "", "{tag}");
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        krusty_errors,
        "{tag}"
    );
    assert_eq!(
        common::rendered_error_blocks(&result.reference_stderr, true),
        kotlinc_errors,
        "{tag}"
    );
    assert_eq!(
        common::compiler_warnings(&result.krusty_stderr),
        [],
        "{tag}"
    );
    assert_eq!(
        common::compiler_warnings(&result.reference_stderr),
        [],
        "{tag}"
    );
    assert_eq!(
        common::compiler_module_warnings(&result.krusty_stderr),
        Vec::<String>::new(),
        "{tag}"
    );
    assert_eq!(
        common::compiler_module_warnings(&result.reference_stderr),
        Vec::<String>::new(),
        "{tag}"
    );
}

fn hide_method_parameters_attribute(library: &std::path::Path, classes: &[(String, Vec<u8>)]) {
    let (_, original) = classes
        .iter()
        .find(|(name, _)| name == "ConstructorOuter$Inner")
        .expect("javac emits the non-static inner class");
    let mut bytes = original.clone();
    let attribute_name = b"MethodParameters";
    let matches = bytes
        .windows(attribute_name.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == attribute_name).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "javac fixture has one MethodParameters UTF8 entry"
    );
    bytes[matches[0]] = b'X';
    std::fs::write(library.join("ConstructorOuter$Inner.class"), &bytes)
        .expect("replace the fixture with an equivalent class lacking MethodParameters");

    let parsed = krusty::jvm::classreader::parse_class(&bytes).expect("parse patched inner class");
    let constructor = parsed
        .method(
            "<init>",
            "(LConstructorOuter;Ljava/lang/String;Ljava/lang/String;)V",
        )
        .expect("inner constructor");
    assert_eq!(
        constructor.parameter_nullability,
        vec![
            None,
            Some(krusty::jvm::classreader::JavaNullability::NotNull),
            None
        ]
    );
}

fn assert_classfile_type_use_qualifiers(retention: &str) {
    let mut java = annotation_sources(retention).to_vec();
    java.push(("Api.java".to_string(), API.to_string()));
    let (library, classes) =
        common::javac_compile(&java, &[]).expect("javac compiles the Java API");
    hide_method_parameters_attribute(&library, &classes);
    let result = common::compiler_diagnostics(&[("Use.kt", USE)], &[library]);
    assert_rejected_like_kotlinc(
        &result,
        &format!("{retention} classfile type-use nullability"),
        true,
    );
}

fn assert_java_source_type_use_qualifiers(retention: &str) {
    let [(non_null_name, non_null), (nullable_name, nullable)] = annotation_sources(retention);
    let (annotations, _) =
        common::javac_compile(&[(non_null_name, non_null), (nullable_name, nullable)], &[])
            .expect("javac compiles the annotations");
    let result =
        common::compiler_diagnostics(&[("Api.java", API), ("Use.kt", USE)], &[annotations]);
    assert_rejected_like_kotlinc(
        &result,
        &format!("{retention} Java source type-use nullability"),
        false,
    );
}

#[test]
fn visible_classfile_type_use_qualifiers_keep_their_paths() {
    assert_classfile_type_use_qualifiers("RUNTIME");
}

#[test]
fn invisible_classfile_type_use_qualifiers_keep_their_paths() {
    assert_classfile_type_use_qualifiers("CLASS");
}

#[test]
fn visible_java_source_type_use_qualifiers_keep_their_paths() {
    assert_java_source_type_use_qualifiers("RUNTIME");
}

#[test]
fn invisible_java_source_type_use_qualifiers_keep_their_paths() {
    assert_java_source_type_use_qualifiers("CLASS");
}
