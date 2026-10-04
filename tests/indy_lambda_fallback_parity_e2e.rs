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
//! The reference recipe mirrors how the Kotlin repository builds `core:util.runtime` (its
//! `@Metadata` stamp is `mv=[2,2,0]`): `-language-version 2.2 -api-version 2.2`, under which the
//! specialized invoke is typed by the lambda body's inferred result. krusty compiles its
//! implemented semantics and stamps `mv=[2,2,0]` through its internal `-Xmetadata-version`, so the
//! classes compare byte for byte.

use super::common;

const LANGUAGE_VERSION_2_2: &[&str] = &["-language-version", "2.2", "-api-version", "2.2"];

const NOTHING_RESULT_SRC: &str = "private val ALWAYS_NULL: (Any?) -> Any? = { null }\n\
    private val BOOM: () -> String = { throw IllegalStateException(\"x\") }\n\
    fun box(): String = if (ALWAYS_NULL(\"x\") == null) \"OK\" else \"fail\"\n";

/// kotlinc and krusty write `class` byte for byte alike, the facade included.
fn assert_identical(src: &str, stem: &str, class: &str) {
    let built = common::compare_with_kotlinc_plugin_metadata_stamp(
        stem,
        src,
        class,
        &[common::stdlib_jar()],
        "1.8",
        &LANGUAGE_VERSION_2_2
            .iter()
            .map(|flag| (*flag).to_string())
            .collect::<Vec<_>>(),
        [2, 2, 0],
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
