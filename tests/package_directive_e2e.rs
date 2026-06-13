//! A `package` directive may follow file-level annotations (`@file:...`); krusty accepts it in the
//! top-level loop (not just as the very first token), recording the package. A `typealias` is
//! skipped (not modeled). (End-to-end JVM execution of package'd files is covered by the box
//! conformance suite; here we assert clean parse + check + emit.)

use krusty::codegen::emit::{emit_file, file_class_name};
use krusty::diag::DiagSink;
use krusty::lexer::lex;
use krusty::parser::parse;
use krusty::resolve::{check_file, collect_signatures};

const SRC: &str = r#"
@file:JvmName("Demo")
package demo.test

typealias Ints = IntArray

fun sum(a: IntArray): Int {
    var s = 0
    for (x in a) s += x
    return s
}
fun box(): String = if (sum(intArrayOf(1, 2, 3)) == 6) "OK" else "fail"
"#;

#[test]
fn package_after_file_annotation_parses_and_emits() {
    let mut d = DiagSink::new();
    let toks = lex(SRC, &mut d);
    let file = parse(SRC, &toks, &mut d);
    // The `package` (after the `@file:` annotation) and the `typealias` are both consumed cleanly.
    assert_eq!(file.package.as_deref(), Some("demo.test"));
    assert!(!d.has_errors(), "parse errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let files = vec![file];
    let syms = collect_signatures(&files, &mut d);
    let info = check_file(&files[0], &syms, &mut d);
    assert!(!d.has_errors(), "check errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());

    let internal = file_class_name("Demo", files[0].package.as_deref());
    assert_eq!(internal, "demo/test/DemoKt"); // facade name (krusty ignores @file:JvmName)
    let _ = emit_file(&files[0], &info, &syms, &internal, &mut d);
    assert!(!d.has_errors(), "emit errors: {:?}", d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>());
}
