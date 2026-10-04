//! `@OptionalExpectation` annotation classes reach a JVM compilation only through the optional
//! annotation section of a library's `META-INF/*.kotlin_module`, as kotlinc's
//! `OptionalAnnotationClassesProvider` reads them. Common sources may apply them: the JVM output
//! carries no trace of the class, but the declaration's `@Metadata` flags still report
//! `hasAnnotations`. Platform sources are rejected with kotlinc's diagnostic.

use super::common;

const PLATFORM: &str = "package optional\n\
    \n\
    fun box(): String = read(Registry.Companion)\n";

/// Both compilers build `common` as the common source of one multiplatform module beside a platform
/// file, and every class they write is byte-identical.
fn assert_common_use_matches_kotlinc(common_source: &str) {
    assert_source_set_matches_kotlinc(common_source, PLATFORM);
}

/// [`assert_common_use_matches_kotlinc`] with the platform file's own `box()`.
fn assert_source_set_matches_kotlinc(common_source: &str, platform_source: &str) {
    let classes = common::classes_against_kotlinc_source_set(
        &[
            ("Common.kt", common_source),
            ("Platform.kt", platform_source),
        ],
        1,
    );
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}

#[test]
fn an_optional_annotation_on_a_common_function_matches_kotlinc() {
    assert_common_use_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload(val text: String)\n\
         \n\
         class Registry {\n\
         \x20   companion object {\n\
         \x20       @kotlin.js.JsStatic\n\
         \x20       fun create(): Payload = Payload(\"OK\")\n\
         \x20   }\n\
         }\n\
         \n\
         fun read(registry: Registry.Companion): String = registry.create().text\n",
    );
}

#[test]
fn an_optional_annotation_on_a_common_property_matches_kotlinc() {
    assert_common_use_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload(val text: String)\n\
         \n\
         class Registry {\n\
         \x20   companion object {\n\
         \x20       @kotlin.js.JsStatic\n\
         \x20       val payload: Payload = Payload(\"OK\")\n\
         \x20   }\n\
         }\n\
         \n\
         fun read(registry: Registry.Companion): String = registry.payload.text\n",
    );
}

const PAYLOAD_PLATFORM: &str = "package optional\n\
    \n\
    fun box(): String = read(make())\n";

#[test]
fn an_optional_annotation_on_a_common_top_level_function_matches_kotlinc() {
    assert_source_set_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload(val text: String)\n\
         \n\
         @kotlin.js.JsName(\"makePayload\")\n\
         fun make(): Payload = Payload(\"OK\")\n\
         \n\
         fun read(payload: Payload): String = payload.text\n",
        PAYLOAD_PLATFORM,
    );
}

#[test]
fn an_optional_annotation_on_a_common_class_matches_kotlinc() {
    assert_source_set_matches_kotlinc(
        "package optional\n\
         \n\
         @kotlin.js.JsName(\"Box\")\n\
         class Payload(val text: String)\n\
         \n\
         fun make(): Payload = Payload(\"OK\")\n\
         \n\
         fun read(payload: Payload): String = payload.text\n",
        PAYLOAD_PLATFORM,
    );
}

#[test]
fn an_optional_annotation_on_a_common_primary_constructor_matches_kotlinc() {
    assert_source_set_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload @kotlin.js.JsName(\"primary\") constructor(val text: String)\n\
         \n\
         fun make(): Payload = Payload(\"OK\")\n\
         \n\
         fun read(payload: Payload): String = payload.text\n",
        PAYLOAD_PLATFORM,
    );
}

#[test]
fn an_optional_annotation_on_a_common_secondary_constructor_matches_kotlinc() {
    assert_source_set_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload(val text: String) {\n\
         \x20   @kotlin.js.JsName(\"secondary\")\n\
         \x20   constructor(first: Payload, second: Payload) : this(first.text + second.text)\n\
         }\n\
         \n\
         fun make(): Payload = Payload(Payload(\"O\"), Payload(\"K\"))\n\
         \n\
         fun read(payload: Payload): String = payload.text\n",
        PAYLOAD_PLATFORM,
    );
}

#[test]
fn an_optional_annotation_on_a_common_value_parameter_matches_kotlinc() {
    assert_source_set_matches_kotlinc(
        "package optional\n\
         \n\
         class Payload(val text: String)\n\
         \n\
         fun make(): Payload = Payload(\"OK\")\n\
         \n\
         @OptIn(kotlin.experimental.ExperimentalObjCName::class)\n\
         fun read(@kotlin.native.ObjCName(\"source\") payload: Payload): String = payload.text\n",
        PAYLOAD_PLATFORM,
    );
}

/// A platform source applying an optional annotation is rejected: kotlinc reports
/// OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE at the annotation's type reference.
#[test]
fn an_optional_annotation_in_a_platform_source_is_rejected_like_kotlinc() {
    use krusty::kotlin_version::KotlinVersion;
    const MESSAGE: &str =
        "Plat.kt:3:10: declaration annotated with '@OptionalExpectation' can only \
        be used in common module sources.";
    let expected: &[(KotlinVersion, &[&str])] = &[
        (KotlinVersion::V2_4_0, &[MESSAGE]),
        (KotlinVersion::V2_4_10, &[MESSAGE]),
        (KotlinVersion::V2_4_20, &[MESSAGE]),
    ];
    let target = krusty::kotlin_version::target();
    let (_, ledger) = expected
        .iter()
        .find(|(version, _)| *version == target)
        .unwrap_or_else(|| panic!("no expected ledger for kotlinc {target}"));
    let sources = [(
        "Plat.kt",
        "class Registry {\n\
         \x20   companion object {\n\
         \x20       @kotlin.js.JsStatic\n\
         \x20       fun create(): Payload = Payload()\n\
         \x20   }\n\
         }\n\
         \n\
         class Payload\n",
    )];
    assert_eq!(common::reference_error_ledger(&sources, &[]), *ledger);
    common::assert_errors_match_kotlinc(&sources, &[]);
}
