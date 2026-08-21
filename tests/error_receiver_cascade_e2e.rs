//! Exact diagnostics for error-typed receivers and arguments.

use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    common::front_end_diagnostics_files_with_stdlib(&[src])
}

#[test]
fn member_access_on_parameter_typed_by_unresolved_classifier_reports_the_member_names() {
    let diags = diagnostics(
        "fun paramCase(u: Unresolved) {\n\
         \u{20}   u.method()\n\
         \u{20}   println(u.name)\n\
         }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
            "unresolved reference 'name'.".to_string(),
        ],
    );
}

#[test]
fn member_access_on_loop_variable_typed_by_unresolved_classifier_reports_the_member_names() {
    let diags = diagnostics(
        "fun loopCase(spans: Collection<Unresolved>) {\n\
         \u{20}   for (span in spans) {\n\
         \u{20}       span.method()\n\
         \u{20}       println(span.name)\n\
         \u{20}   }\n\
         }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
            "unresolved reference 'name'.".to_string(),
        ],
    );
}

#[test]
fn member_access_on_failed_receiver_expression_adds_no_member_cascade() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val x = missingFn().foo\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn error_typed_argument_adds_no_top_level_overload_ambiguity() {
    let diags = diagnostics(
        "fun single(x: Any?) = x\n\
                             \n\
                             fun failedCallArg() {\n\
                             \u{20}   println(missingFn())\n\
                             \u{20}   single(missingFn())\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'missingFn'.".to_string(),
            "unresolved reference 'missingFn'.".to_string(),
        ],
    );
}

#[test]
fn error_typed_argument_adds_no_member_overload_ambiguity() {
    let diags = diagnostics(
        "fun memberOverloadArg(sb: StringBuilder) {\n\
                             \u{20}   sb.append(missingFn())\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn genuine_overload_ambiguity_still_reports() {
    // kotlinc 2.4.10: `overload resolution ambiguity between candidates` — both overloads accept
    // `(null, null)` and neither is more specific. No argument is error-typed, so nothing is
    // suppressed.
    let diags = diagnostics(
        "fun h(a: Int?, b: String?) {}\n\
                             fun h(a: String?, b: Int?) {}\n\
                             fun useH() { h(null, null) }\n",
    );
    assert_eq!(diags, vec!["overload resolution ambiguity".to_string()]);
}

#[test]
fn local_from_failed_expression_stays_silent_on_member_access() {
    let diags = diagnostics(
        "fun a() {\n\
                             \u{20}   val v = missingFn()\n\
                             \u{20}   v.method()\n\
                             \u{20}   println(v.name)\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn local_from_failed_member_call_stays_silent_on_member_access() {
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   val v = u.method()\n\
                             \u{20}   v.prop\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
        ],
    );
}

#[test]
fn local_copy_of_declared_error_value_keeps_reporting_member_access() {
    // The initializer `u` itself checks cleanly — the error lives at u's declared type — so the
    // copy keeps the declared-type flavor and member accesses on it report.
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   val v = u\n\
                             \u{20}   v.method()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
        ],
    );
}

#[test]
fn member_read_of_declared_error_property_keeps_reporting() {
    let diags = diagnostics(
        "class C {\n\
                             \u{20}   val p: Unresolved = TODO()\n\
                             }\n\
                             fun f(c: C) {\n\
                             \u{20}   c.p.method()\n\
                             \u{20}   val v = c.p\n\
                             \u{20}   v.method()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
            "unresolved reference 'method'.".to_string(),
        ],
    );
}

#[test]
fn any_members_bind_on_error_receiver_without_diagnostic() {
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   val s: String = u.toString()\n\
                             \u{20}   val h: Int = u.hashCode()\n\
                             \u{20}   val e: Boolean = u.equals(null)\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec!["unresolved reference 'Unresolved'.".to_string()]
    );
}

#[test]
fn any_member_binding_on_error_receiver_carries_its_real_type() {
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   val s = u.toString()\n\
                             \u{20}   val bad: Int = s\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "initializer type mismatch: expected 'Int', actual 'String'.".to_string(),
        ],
    );
}

#[test]
fn in_scope_extension_binds_on_error_receiver_with_its_real_type() {
    let diags = diagnostics(
        "class Foo\n\
                             fun Foo.myExt(): Int = 1\n\
                             fun a(u: Unresolved) {\n\
                             \u{20}   val i = u.myExt()\n\
                             \u{20}   val bad: String = i\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "initializer type mismatch: expected 'String', actual 'Int'.".to_string(),
        ],
    );
}

#[test]
fn multiple_in_scope_extensions_bind_the_first_silently() {
    // kotlinc picks the first-declared candidate at the nearest scope level with NO ambiguity
    // report: flipping the declaration order flips the bound type (verified against kotlinc).
    let diags = diagnostics(
        "fun String.dup(): Int = 1\n\
                             fun Int.dup(): Long = 2\n\
                             fun a(u: Unresolved) {\n\
                             \u{20}   val r = u.dup()\n\
                             \u{20}   val bad: String = r\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "initializer type mismatch: expected 'String', actual 'Int'.".to_string(),
        ],
    );
}

#[test]
fn stdlib_extension_and_extension_property_bind_silently_on_error_receiver() {
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   val b = u.isEmpty()\n\
                             \u{20}   val i = u.indices\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec!["unresolved reference 'Unresolved'.".to_string()]
    );
}

#[test]
fn arguments_of_error_receiver_call_are_checked_but_never_matched() {
    // `missingArg` is diagnosed on its own; the arity mismatch of `u.toString(1)` and the missing
    // argument of `u.equals()` add nothing (kotlinc binds the candidate regardless).
    let diags = diagnostics(
        "fun a(u: Unresolved) {\n\
                             \u{20}   u.toString(missingArg)\n\
                             \u{20}   u.toString(1)\n\
                             \u{20}   val b: Boolean = u.equals()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'missingArg'.".to_string(),
        ],
    );
}

#[test]
fn expression_error_local_also_binds_any_members_silently() {
    let diags = diagnostics(
        "fun a() {\n\
                             \u{20}   val v = missingFn()\n\
                             \u{20}   val s: String = v.toString()\n\
                             \u{20}   val b: Boolean = v.isEmpty()\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn not_on_declared_error_operand_reports() {
    let diags = diagnostics(
        "fun f(u: Unresolved) {\n\
                             \u{20}   val a = !u\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
        ],
    );
}

#[test]
fn not_on_failed_call_operand_stays_silent() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val a = !missingFn()\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn not_on_diagnosed_root_name_stays_silent() {
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   val a = !attributes\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
        ],
    );
}

#[test]
fn not_on_property_read_bound_on_expression_error_receiver_reports() {
    // `attributes.isEmpty` binds the in-scope `isEmpty` FUNCTION in property position (kotlinc
    // binds it — on a declared receiver it complains `function invocation 'isEmpty()' expected`),
    // leaving an unmarked error that `!` reports.
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   if (!attributes.isEmpty) { }\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
        ],
    );
}

#[test]
fn not_on_call_bound_on_expression_error_receiver_reports() {
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val events = span.events\n\
                             \u{20}   if (!events.isEmpty()) { }\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'events'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
        ],
    );
}

#[test]
fn not_on_local_from_silent_bind_on_expression_error_receiver_reports() {
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   val w = attributes.isEmpty()\n\
                             \u{20}   val b = !w\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
        ],
    );
}

#[test]
fn not_on_silent_bind_over_declared_root_reports() {
    let diags = diagnostics(
        "fun f(u: Unresolved) {\n\
                             \u{20}   val w = u.isEmpty()\n\
                             \u{20}   val b = !w\n\
                             \u{20}   val c = !u.isEmpty()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
            "unresolved reference 'not' for operator '!'.".to_string(),
        ],
    );
}

#[test]
fn not_on_non_binding_cascade_chain_stays_silent() {
    // Neither `asMap` nor `size` names any in-scope candidate, so the silent accesses keep the
    // diagnosed mark of `attributes` and `!` adds nothing.
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   val b = !attributes.asMap().size\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
        ],
    );
}

#[test]
fn copy_of_expression_error_local_keeps_the_diagnosed_mark() {
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   val a2 = attributes\n\
                             \u{20}   val b = !a2\n\
                             \u{20}   a2.method()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
        ],
    );
}

#[test]
fn local_from_non_binding_cascade_keeps_the_diagnosed_mark() {
    let diags = diagnostics(
        "fun f(span: Unresolved) {\n\
                             \u{20}   val attributes = span.attributes\n\
                             \u{20}   val w2 = attributes.asMap()\n\
                             \u{20}   val b = !w2\n\
                             \u{20}   w2.method()\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'attributes'.".to_string(),
        ],
    );
}
