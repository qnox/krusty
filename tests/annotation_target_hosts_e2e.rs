//! An annotation applied to a declaration whose kind its `@Target` does not list is a
//! WRONG_ANNOTATION_TARGET error naming that declaration kind (kotlinc).

use super::common;
use super::diagnostics_parity_support::errors;

const SOURCE: &str = "@Target(AnnotationTarget.CONSTRUCTOR)
annotation class OnlyConstructor

@Target(AnnotationTarget.FUNCTION)
annotation class OnlyFunction

@Target(AnnotationTarget.CLASS)
annotation class OnlyClass

@OnlyConstructor
annotation class Derived

@OnlyConstructor
class Plain(@OnlyConstructor val x: Int) {
    @OnlyConstructor constructor() : this(0)
    @OnlyClass
    fun member() {}
}

@OnlyConstructor
enum class Colour @OnlyConstructor constructor() {
    @OnlyConstructor RED,
}

@OnlyFunction
interface Face

@OnlyFunction
object Single

@OnlyClass
fun top(@OnlyClass y: @OnlyClass Int): Int {
    @OnlyClass
    fun local(@OnlyClass z: Int) = z + 1
    @OnlyClass
    val inner = local(y)
    return inner
}
";

#[test]
fn an_annotation_on_a_declaration_its_targets_do_not_list_is_rejected_like_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("TargetHosts.kt", SOURCE)], std::slice::from_ref(&stdlib));
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
            .map(|error| (error.line, error.column, error.message.as_str()))
            .collect::<Vec<_>>(),
        [
            (10, 1, "this annotation is not applicable to target 'annotation class'. Applicable targets: constructor"),
            (13, 1, "this annotation is not applicable to target 'class'. Applicable targets: constructor"),
            (14, 13, "this annotation is not applicable to target 'value parameter'. Applicable targets: constructor"),
            (16, 5, "this annotation is not applicable to target 'member function'. Applicable targets: class"),
            (20, 1, "this annotation is not applicable to target 'enum class'. Applicable targets: constructor"),
            (22, 5, "this annotation is not applicable to target 'enum entry'. Applicable targets: constructor"),
            (25, 1, "this annotation is not applicable to target 'interface'. Applicable targets: function"),
            (28, 1, "this annotation is not applicable to target 'standalone object'. Applicable targets: function"),
            (31, 1, "this annotation is not applicable to target 'top level function'. Applicable targets: class"),
            (32, 9, "this annotation is not applicable to target 'value parameter'. Applicable targets: class"),
            (32, 23, "this annotation is not applicable to target 'type usage'. Applicable targets: class"),
            (33, 5, "this annotation is not applicable to target 'local function'. Applicable targets: class"),
            (34, 15, "this annotation is not applicable to target 'value parameter'. Applicable targets: class"),
            (35, 5, "this annotation is not applicable to target 'local variable'. Applicable targets: class"),
        ]
    );
}
