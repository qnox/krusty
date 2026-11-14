//! kotlinc's WRONG_ANNOTATION_TARGET on an `init` block: an initializer is a declaration that no
//! annotation target denotes, so every annotation written before `init` is rejected at its `@`,
//! listing the annotation class's targets. Its arguments are still resolved in the class body,
//! instance members included, and checked as constants.

use super::common;
use super::diagnostics_parity_support::errors;

fn assert_errors_match_kotlinc(file: &str, source: &str, expected: &[&str]) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[(file, source)], std::slice::from_ref(&stdlib));
    assert_ne!(result.krusty_code, 0, "krusty silently accepted source");
    assert_ne!(
        result.reference_code, 0,
        "kotlinc unexpectedly accepted source"
    );
    let mut krusty_errors = errors(&result.krusty_stderr);
    krusty_errors.extend(errors(&result.krusty_stdout));
    let kotlinc_errors = errors(&result.reference_stderr);
    assert_eq!(krusty_errors, kotlinc_errors);
    assert_eq!(
        krusty_errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}

const DEFAULT_TARGETS: &str = "this annotation is not applicable to target 'initializer'. \
    Applicable targets: class, annotation class, property, field, local variable, value \
    parameter, constructor, function, getter, setter, backing field";

#[test]
fn annotations_on_initializers_are_rejected_like_kotlinc() {
    let source = "annotation class Anno(val position: String)\n\
                  @Target(AnnotationTarget.FUNCTION, AnnotationTarget.CLASS)\n\
                  annotation class Marker\n\
                  class MyClass {\n\
                  \x20   @Anno(\"init $prop\") init {\n\
                  \x20   }\n\
                  \x20   @Marker\n\
                  \x20   init { println() }\n\
                  \x20   val own = 1\n\
                  \x20   @Anno(\"own $own\") init {}\n\
                  \x20   companion object {\n\
                  \x20       private const val prop = 0\n\
                  \x20   }\n\
                  }\n";
    assert_errors_match_kotlinc(
        "InitializerAnnotations.kt",
        source,
        &[
            DEFAULT_TARGETS,
            "this annotation is not applicable to target 'initializer'. Applicable targets: \
             function, class",
            DEFAULT_TARGETS,
            "only 'const val' can be used in constant expressions.",
        ],
    );
}

#[test]
fn an_initializer_annotation_in_a_local_class_checks_its_arguments_like_kotlinc() {
    let source = "annotation class Anno(val position: String)\n\
                  fun foo() {\n\
                  \x20   class MyClass {\n\
                  \x20       val prop = 0\n\
                  \x20       @Anno(\"init $prop\")  init {\n\
                  \x20       }\n\
                  \x20   }\n\
                  }\n";
    assert_errors_match_kotlinc(
        "LocalInitializerAnnotations.kt",
        source,
        &[
            DEFAULT_TARGETS,
            "only 'const val' can be used in constant expressions.",
        ],
    );
}

#[test]
fn use_site_annotations_on_initializers_are_rejected_like_kotlinc() {
    let source = "@Target(AnnotationTarget.FIELD, AnnotationTarget.PROPERTY_GETTER, \
                  AnnotationTarget.VALUE_PARAMETER)\n\
                  annotation class Anno\n\
                  class C {\n\
                  \x20   @field:Anno init {}\n\
                  \x20   @get:Anno init {}\n\
                  \x20   @param:Anno init {}\n\
                  }\n";
    let wrong_target = |site: &str| {
        format!(
            "this annotation is not applicable to target 'initializer' and use-site target \
             '@{site}'. Applicable targets: field, getter, value parameter"
        )
    };
    let field = wrong_target("field");
    let get = wrong_target("get");
    let param = wrong_target("param");
    assert_errors_match_kotlinc(
        "UseSiteInitializerAnnotations.kt",
        source,
        &[
            field.as_str(),
            "'@get:' annotations can only be applied to property declarations.",
            get.as_str(),
            "'@param:' annotations can only be applied to primary constructor parameters.",
            param.as_str(),
        ],
    );
}
