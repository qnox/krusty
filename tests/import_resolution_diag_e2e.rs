//! Import directives resolve independently of their uses. Each failing import emits one exact
//! diagnostic for its first unresolved segment; importable callables, classifier members, and
//! aliases remain silent.

use super::common;
use krusty::diag::DiagSink;
use krusty::frontend::analyze_source_set;
use krusty::libraries::EmptySymbolSource;

fn diagnostics(src: &str) -> Vec<String> {
    let mut diags = common::front_end_diagnostics_files_with_stdlib(&[src]);
    diags.sort();
    diags
}

fn diagnostics_files(sources: &[&str]) -> Vec<String> {
    let mut diags = common::front_end_diagnostics_files_with_stdlib(sources);
    diags.sort();
    diags
}

fn diagnostics_with_spans(sources: &[&str]) -> Vec<(u32, u32, u32, String)> {
    let mut diagnostics = DiagSink::new();
    analyze_source_set(sources, Box::new(EmptySymbolSource), &mut diagnostics);
    diagnostics
        .diags
        .into_iter()
        .map(|diagnostic| {
            (
                diagnostic.file,
                diagnostic.span.lo,
                diagnostic.span.hi,
                diagnostic.msg,
            )
        })
        .collect()
}

#[test]
fn last_segment_failure_reports_the_terminal_segment() {
    let diags = diagnostics("import java.util.Nonexistent\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'Nonexistent'."]);
}

#[test]
fn failure_span_is_the_exact_unresolved_segment() {
    let diags = diagnostics_with_spans(&[
        "package api\nclass Existing\n",
        "import api.Existing.Missing\nfun f() = 1\n",
    ]);
    assert_eq!(
        diags,
        vec![(1, 20, 27, "unresolved reference 'Missing'.".to_string(),)]
    );
}

#[test]
fn first_segment_failure_reports_the_root_segment() {
    let diags = diagnostics("import nonexistent.pkg.*\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'nonexistent'."]);
}

#[test]
fn missing_first_segment_of_a_long_path_reports_the_root() {
    let diags = diagnostics("import io.opentelemetry.api.common.Attributes\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'io'."]);
}

#[test]
fn aliased_import_reports_the_failing_path_segment() {
    let diags = diagnostics("import io.opentelemetry.api.common.Attributes as Attr\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'io'."]);
}

#[test]
fn each_bad_import_reports_exactly_once() {
    let diags = diagnostics(
        "import java.util.Nonexistent\n\
         import nonexistent.pkg.*\n\
         fun f() = 1\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Nonexistent'.",
            "unresolved reference 'nonexistent'.",
        ]
    );
}

#[test]
fn an_unused_bad_import_is_still_reported() {
    let diags = diagnostics("import java.util.Missing\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'Missing'."]);
}

#[test]
fn a_used_bad_import_reports_both_the_import_and_the_use() {
    let diags = diagnostics("import java.util.Nonexistent\nfun f(x: Nonexistent) = 1\n");
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Nonexistent'.",
            "unresolved reference 'Nonexistent'.",
        ]
    );
}

#[test]
fn valid_imports_are_silent() {
    let diags = diagnostics(
        "import java.util.ArrayList\n\
         import kotlin.String\n\
         import kotlin.collections.*\n\
         fun f(a: ArrayList<String>, s: String) = a.size + s.length\n",
    );
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn top_level_callable_import_is_silent() {
    let diags = diagnostics("import kotlin.collections.listOf\nfun f() = listOf(1)\n");
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn valid_aliased_classifier_import_is_silent() {
    let diags =
        diagnostics("import java.util.ArrayList as List\nfun f(): List<String> = List<String>()\n");
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn source_top_level_callable_and_property_imports_are_silent() {
    let diags = diagnostics_files(&[
        "package api\nfun answer() = 42\nconst val FLAG = 1\n",
        "import api.answer\nimport api.FLAG\nfun f() = answer() + FLAG\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn source_typealias_import_is_silent() {
    let diags = diagnostics_files(&[
        "package api\ntypealias Count = Int\n",
        "import api.Count\nfun f(value: Count) = value\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn missing_top_level_callable_reports_the_terminal_segment() {
    let diags = diagnostics("import kotlin.collections.nonexistentFun\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'nonexistentFun'."]);
}

#[test]
fn companion_object_import_is_silent() {
    let diags = diagnostics("import kotlin.String.Companion\nfun f() = 1\n");
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn class_instance_member_exists_but_is_not_importable() {
    let diags = diagnostics("import kotlin.String.length\nfun f(s: String) = s.length\n");
    assert_eq!(
        diags,
        vec!["cannot import 'length'. Functions and properties can only be imported from packages or objects."]
    );
}

#[test]
fn enum_entry_import_is_silent() {
    let diags = diagnostics_files(&[
        "package mypkg\nenum class SomeEnum { ENTRY, OTHER }\n",
        "import mypkg.SomeEnum.ENTRY\nfun f() = 1\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn missing_enum_entry_reports_the_terminal_segment() {
    let diags = diagnostics_files(&[
        "package mypkg\nenum class SomeEnum { ENTRY, OTHER }\n",
        "import mypkg.SomeEnum.MISSING\nfun f() = 1\n",
    ]);
    assert_eq!(diags, vec!["unresolved reference 'MISSING'."]);
}

#[test]
fn missing_classifier_in_a_member_path_reports_the_classifier() {
    let diags = diagnostics_files(&[
        "package mypkg\nenum class SomeEnum { ENTRY, OTHER }\n",
        "import mypkg.NotAnEnum.ENTRY\nfun f() = 1\n",
    ]);
    assert_eq!(diags, vec!["unresolved reference 'NotAnEnum'."]);
}

#[test]
fn non_terminal_enum_entry_reports_that_segment() {
    let diags = diagnostics_files(&[
        "package mypkg\nenum class SomeEnum { ENTRY, OTHER }\n",
        "import mypkg.SomeEnum.ENTRY.name\nfun f() = 1\n",
    ]);
    assert_eq!(diags, vec!["unresolved reference 'ENTRY'."]);
}

#[test]
fn java_static_member_import_is_silent() {
    let diags = diagnostics(
        "import java.lang.Math.PI\nimport java.lang.Math.abs\nfun f() = abs(PI).toInt()\n",
    );
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn nested_classifier_import_is_silent() {
    let diags = diagnostics("import java.util.Map.Entry\nfun f() = 1\n");
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn missing_nested_classifier_reports_the_terminal_segment() {
    let diags = diagnostics("import java.util.Map.Nope\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'Nope'."]);
}

#[test]
fn missing_member_of_an_existing_class_reports_the_member() {
    let diags = diagnostics("import java.util.ArrayList.Nonexistent\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'Nonexistent'."]);
}

#[test]
fn object_member_import_is_silent() {
    let diags = diagnostics_files(&[
        "package opp\nobject Holder { const val K = 1; fun answer() = 42 }\n",
        "import opp.Holder.K\nimport opp.Holder.answer\nfun f() = K + answer()\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn companion_member_import_is_silent() {
    let diags = diagnostics_files(&[
        "package api\nclass Holder { companion object { const val K = 1; fun answer() = 42 } }\n",
        "import api.Holder.Companion.K\nimport api.Holder.Companion.answer\nfun f() = K + answer()\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn source_nested_classifier_import_is_silent() {
    let diags = diagnostics_files(&[
        "package api\nclass Outer { class Nested }\n",
        "import api.Outer.Nested\nfun f() = Nested()\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn on_demand_import_from_a_classifier_is_silent() {
    let diags = diagnostics("import kotlin.String.*\nfun f() = 1\n");
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn missing_wildcard_qualifier_reports_the_first_missing_segment() {
    let diags = diagnostics("import java.missing.deep.*\nfun f() = 1\n");
    assert_eq!(diags, vec!["unresolved reference 'missing'."]);
}

#[test]
fn same_package_references_need_no_import() {
    let diags = diagnostics_files(&[
        "package same\nclass Foo\n",
        "package same\nfun f() = Foo()\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}
