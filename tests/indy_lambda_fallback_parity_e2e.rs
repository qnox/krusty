//! Under `-Xlambdas=indy` a lambda `LambdaMetafactory` cannot adapt compiles to a class of its
//! own (kotlinc's `LambdaMetafactoryArguments` failure paths), not an `invokedynamic`.
//!
//! The covered conflict, against kotlinc's `LambdaMetafactoryArguments.kt`: a `Nothing`/`Nothing?`
//! type the factory cannot adapt — the specialized invoke's inferred result (`{ null }`,
//! `{ throw … }`) or a parameter of the selected function type (`(Nothing) -> Unit`,
//! `(Nothing?) -> Unit`). `computeParameterTypeAdaptationConstraint` returns CONFLICT
//! (`FunctionHazard`), so the lambda is a singleton class named after its enclosing property, read
//! from its `INSTANCE` field. Whether the class keeps its generic `FunctionN` supertype follows
//! kotlinc's `hasNothingInNonContravariantPosition`: a non-null `Nothing` parameter keeps it (its
//! argument written as a star), a `Nothing?` parameter or a `Nothing`/`Nothing?` result leaves it
//! raw.
//!
//! The behavior is language-level dependent. Kotlin 2.2 types the specialized invoke by the
//! lambda body's inferred result and takes the class fallback; Kotlin 2.4 types it by the expected
//! function result and keeps the lambda on indy. Every comparison supplies the same public
//! language/API settings to kotlinc and krusty.

use super::common;

const NOTHING_RESULT_SRC: &str = "private val ALWAYS_NULL: (Any?) -> Any? = { null }\n\
    private val BOOM: () -> String = { throw IllegalStateException(\"x\") }\n\
    fun box(): String = if (ALWAYS_NULL(\"x\") == null) \"OK\" else \"fail\"\n";

/// kotlinc and krusty write `class` byte for byte alike, the facade included.
fn assert_identical(src: &str, stem: &str, class: &str) {
    let language_settings = krusty::language_settings::LanguageSettings::new(
        krusty::language_version::LanguageVersion::V2_2,
        None,
        &[],
    )
    .expect("Kotlin 2.2 language/API settings");
    let built = common::compare_with_kotlinc_plugin_language_settings(
        stem,
        src,
        class,
        &[common::stdlib_jar()],
        "1.8",
        &language_settings,
    )
    .expect("reference kotlinc is provisioned");
    assert!(!built.reference_bytes.is_empty(), "kotlinc writes {class}");
    assert!(
        built.krusty_bytes == built.reference_bytes,
        "{class} differs from kotlinc's:\n{}\n---\n{}",
        built.krusty,
        built.reference
    );
}

#[test]
fn current_language_keeps_the_null_returning_lambda_on_indy() {
    let settings = krusty::language_settings::LanguageSettings::default();
    let built = common::compare_with_kotlinc_plugin_language_settings(
        "NothingLambdaCurrent",
        NOTHING_RESULT_SRC,
        "NothingLambdaCurrentKt",
        &[common::stdlib_jar()],
        "1.8",
        &settings,
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(built.krusty_bytes, built.reference_bytes);

    let classes = common::source_set_compile::compile(
        &[("NothingLambdaCurrent.kt", NOTHING_RESULT_SRC)],
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
        Some(52),
        Some(settings.language_version.metadata_version()),
        &settings,
    )
    .expect("krusty compiles the current-language fixture");
    let class_names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert!(
        class_names
            .iter()
            .all(|name| !name.contains("$ALWAYS_NULL$") && !name.contains("$BOOM$")),
        "Kotlin 2.4 keeps the Nothing-result lambdas on indy: {class_names:?}"
    );
}

#[test]
fn a_null_returning_lambda_is_a_singleton_class_named_after_its_property() {
    assert_identical(
        NOTHING_RESULT_SRC,
        "NothingLambda",
        "NothingLambdaKt$ALWAYS_NULL$1",
    );
}

#[test]
fn a_throwing_lambda_is_a_singleton_class_named_after_its_property() {
    assert_identical(
        NOTHING_RESULT_SRC,
        "NothingLambda",
        "NothingLambdaKt$BOOM$1",
    );
}

#[test]
fn the_facade_reads_the_instance_field_instead_of_a_bootstrap() {
    assert_identical(NOTHING_RESULT_SRC, "NothingLambda", "NothingLambdaKt");
}

#[test]
fn nothing_result_lambdas_run() {
    common::expect_box_same_as_kotlinc(NOTHING_RESULT_SRC, "NothingLambda");
}

const NOTHING_PARAM_SRC: &str = "private val UNREACHABLE: (Nothing) -> Unit = { n -> }\n\
    private val MAYBE_UNREACHABLE: (Nothing?) -> Unit = { n -> }\n\
    fun box(): String = \"OK\"\n";

/// A non-null `Nothing` parameter is an `in` argument: the class keeps its generic supertype,
/// the `Nothing` argument written as a star (`Function1<*Lkotlin/Unit;>;`).
#[test]
fn a_nothing_parameter_keeps_the_generic_supertype_with_a_star_argument() {
    assert_identical(
        NOTHING_PARAM_SRC,
        "NothingParam",
        "NothingParamKt$UNREACHABLE$1",
    );
}

/// A `Nothing?` parameter is a nullable-Nothing argument: the supertype is raw.
#[test]
fn a_nullable_nothing_parameter_leaves_the_supertype_raw() {
    assert_identical(
        NOTHING_PARAM_SRC,
        "NothingParam",
        "NothingParamKt$MAYBE_UNREACHABLE$1",
    );
}

#[test]
fn the_param_fixture_facade_reads_the_instance_fields() {
    assert_identical(NOTHING_PARAM_SRC, "NothingParam", "NothingParamKt");
}

const CLASSPATH_INLINE_LIB: &str = "package lib\n\
    public inline fun makeNothingLambda(): (Any?) -> Any? = { null }\n";
const CLASSPATH_INLINE_USE: &str = "import lib.makeNothingLambda\n\
    private val VALUE: (Any?) -> Any? = makeNothingLambda()\n\
    fun box(): String = if (VALUE(\"x\") == null) \"OK\" else \"fail\"\n";

/// A lambda inside a dependency inline body is already class-shaped. The bytecode inliner copies
/// that class at the call site under both language modes; it is not a source lambda that needs the
/// pre-2.4 inferred-result fact consumed by `nothing_conflict`.
#[test]
fn a_classpath_inline_nothing_lambda_is_regenerated_at_both_language_levels() {
    for version in [
        krusty::language_version::LanguageVersion::V2_2,
        krusty::language_version::LanguageVersion::V2_4,
    ] {
        let settings = krusty::language_settings::LanguageSettings::new(version, None, &[])
            .expect("supported language/API settings");
        let classes = common::classes_against_kotlinc_lib_language_settings(
            "Use",
            &[("Lib.kt", CLASSPATH_INLINE_LIB)],
            CLASSPATH_INLINE_USE,
            &settings,
        )
        .expect("reference kotlinc is provisioned");
        assert_eq!(
            classes.reference.keys().collect::<Vec<_>>(),
            ["UseKt", "UseKt$special$$inlined$makeNothingLambda$1"],
            "kotlinc {version} class inventory"
        );
        let differences = classes.differences();
        assert!(
            differences.is_empty(),
            "language {version}: {}",
            differences.join("\n\n")
        );
    }
}
