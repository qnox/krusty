//! Diagnostic ownership for failed property type inference.

use super::common;

fn assert_diagnostics(name: &str, src: &str, expected: &[&str]) {
    let diagnostics = common::front_end_diagnostics(src, &[], None);
    assert_eq!(
        diagnostics.len(),
        expected.len(),
        "{name}: diagnostic count, got {diagnostics:?}"
    );
    assert_eq!(diagnostics, expected, "{name}: exact diagnostics");
}

#[test]
fn toplevel_eager_initializer_with_unresolved_call_reports_only_the_reference() {
    assert_diagnostics(
        "top-level eager initializer",
        "package sample\nclass Foo\nprivate val LOG = logger<Foo>()\n",
        &["unresolved reference 'logger'."],
    );
}

#[test]
fn member_eager_initializer_with_unresolved_call_reports_only_the_reference() {
    assert_diagnostics(
        "member eager initializer",
        "package sample\nclass C {\n    private val LOG = logger<C>()\n}\n",
        &["unresolved reference 'logger'."],
    );
}

#[test]
fn expression_getter_with_unresolved_call_reports_only_the_reference() {
    assert_diagnostics(
        "expression getter",
        "package sample\nclass C {\n    val GETTER get() = missingFn()\n}\n",
        &["unresolved reference 'missingFn'."],
    );
}

#[test]
fn top_level_expression_getter_reports_only_the_reference() {
    assert_diagnostics(
        "top-level expression getter",
        "package sample\nval value get() = missingTopExpression()\n",
        &["unresolved reference 'missingTopExpression'."],
    );
}

#[test]
fn block_getters_report_the_missing_type_and_body_reference() {
    assert_diagnostics(
        "block getters",
        "package sample\nval top get() { return missingTopBlock() }\nval String.topExtension get() { return missingTopExtensionBlock() }\nclass C {\n    val member get() { return missingMemberBlock() }\n    val String.memberExtension get() { return missingMemberExtensionBlock() }\n}\n",
        &[
            "this property must have an explicit type, be initialized, or be delegated.",
            "unresolved reference 'missingTopBlock'.",
            "this property must have an explicit type, be initialized, or be delegated.",
            "unresolved reference 'missingTopExtensionBlock'.",
            "this property must have an explicit type, be initialized, or be delegated.",
            "unresolved reference 'missingMemberBlock'.",
            "this property must have an explicit type, be initialized, or be delegated.",
            "unresolved reference 'missingMemberExtensionBlock'.",
        ],
    );
}

#[test]
fn block_getters_with_valid_bodies_report_only_the_missing_types() {
    assert_diagnostics(
        "valid block getter bodies",
        "package sample\nval top get() { return 1 }\nclass C { val member get() { return 2 } }\n",
        &[
            "this property must have an explicit type, be initialized, or be delegated.",
            "this property must have an explicit type, be initialized, or be delegated.",
        ],
    );
}

#[test]
fn extension_getter_failures_report_only_the_body_references() {
    assert_diagnostics(
        "extension getters",
        "package sample\nval String.top get() = missingTopExtension()\nclass C {\n    val String.member get() = missingMemberExtension()\n}\n",
        &[
            "unresolved reference 'missingTopExtension'.",
            "unresolved reference 'missingMemberExtension'.",
        ],
    );
}

#[test]
fn member_property_with_failed_initializer_keeps_member_probes_silent() {
    // kotlinc: `val mw = missingFn()` reports only the unresolved callee; `mw.length` adds nothing.
    assert_diagnostics(
        "member probes on a member property with a failed initializer",
        "package sample\nclass C {\n    val mw = missingFn()\n    fun m() = mw.length\n}\n",
        &["unresolved reference 'missingFn'."],
    );
}

#[test]
fn declared_type_error_still_reports_member_probes() {
    // kotlinc: an unresolved DECLARED type is the other flavor — `v.length` reports the member.
    assert_diagnostics(
        "member probes on a declared-type error",
        "package sample\nval v: Missing = 1\nfun u() = v.length\n",
        &[
            "unresolved reference 'Missing'.",
            "unresolved reference 'length'.",
        ],
    );
}

#[test]
fn eager_failure_reads_of_the_error_typed_property_stay_silent() {
    // kotlinc diagnoses the failed initializer and nothing else: reads of the error-typed
    // property resolve silently (an error type is assignable everywhere and member probes on it
    // stay quiet).
    assert_diagnostics(
        "reads of an error-typed property",
        "package sample\nval a = missingFn()\nval b: Int = a\nfun use() = a.length\n",
        &["unresolved reference 'missingFn'."],
    );
}

#[test]
fn inference_cycle_reports_each_recursive_read() {
    assert_diagnostics(
        "inference cycle",
        "package sample\nval x get() = y\nval y get() = x\n",
        &[
            "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
            "type checking has run into a recursive problem. Easiest workaround: specify the types of your declarations explicitly.",
        ],
    );
}

#[test]
fn eager_forward_reference_reports_the_uninitialized_read() {
    assert_diagnostics(
        "eager forward reference",
        "package sample\nval eager = later\nval later = 1\n",
        &["variable 'later' must be initialized."],
    );
}

#[test]
fn every_eager_forward_read_is_reported_once() {
    assert_diagnostics(
        "multiple eager forward references",
        "package sample\nval eager = later + after\nval later = 1\nval after = 2\n",
        &[
            "variable 'later' must be initialized.",
            "variable 'after' must be initialized.",
        ],
    );
}
