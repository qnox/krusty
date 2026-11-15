//! Java source headers in the analysis worker: Kotlin resolves against stubs of the request's Java
//! sources, which are generated once while the sources and the classpath they read are unchanged.

use std::io::Cursor;

use super::super::java_stub_memo::{java_stub_generations, reset_java_stub_generations};
use super::*;

#[test]
fn worker_resolves_kotlin_reference_to_stubbed_java() {
    let kotlin = "fun use(w: p.Widget) {}";
    let request = format!(
        "{{\"sources\":[{}],\"source_kinds\":[0],\"result_count\":1,\
          \"java_sources\":[{}]}}",
        serde_json::to_string(kotlin).unwrap(),
        serde_json::to_string("package p; public class Widget {}").unwrap(),
    );
    let mut input = Vec::new();
    write_framed(&mut input, request.as_bytes()).unwrap();
    let mut output = Vec::new();
    run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

    let analyses = decode_worker_output(output);
    let diagnostics = &analyses[0].diagnostics;
    assert!(
        !diagnostics.iter().any(|d| d.message.contains("Widget")),
        "Widget resolved from stub: {diagnostics:?}"
    );
}

#[test]
fn unchanged_java_stubs_are_generated_once() {
    reset_java_stub_generations();
    let java = "package p; public class Widget {}".to_string();
    let source = "fun use(w: p.Widget) {}";
    let mut input = Vec::new();
    write_analysis_request(&mut input, source, std::slice::from_ref(&java), None);
    write_analysis_request(&mut input, source, std::slice::from_ref(&java), None);
    write_analysis_request(&mut input, source, &[], None);
    write_analysis_request(&mut input, source, &[java], None);
    let mut output = Vec::new();

    run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

    assert_eq!(java_stub_generations(), 1);
    let analyses = decode_worker_analyses(output, 4);
    assert_eq!(diagnostic_messages(&analyses[0]), Vec::<&str>::new());
    assert_eq!(diagnostic_messages(&analyses[1]), Vec::<&str>::new());
    assert_eq!(
        diagnostic_messages(&analyses[2]),
        vec!["unresolved reference 'p'."]
    );
    assert_eq!(diagnostic_messages(&analyses[3]), Vec::<&str>::new());
}

#[test]
fn a_java_source_change_rebuilds_stubs() {
    reset_java_stub_generations();
    let source = "fun use(w: p.Widget) {}";
    let mut input = Vec::new();
    write_analysis_request(
        &mut input,
        source,
        &["package p; public class Widget {}".to_string()],
        None,
    );
    write_analysis_request(
        &mut input,
        source,
        &["package p; public class Gone {}".to_string()],
        None,
    );
    let mut output = Vec::new();

    run_analysis_worker(&mut Cursor::new(input), &mut output, Vec::new()).unwrap();

    assert_eq!(java_stub_generations(), 2);
    let analyses = decode_worker_analyses(output, 2);
    assert_eq!(diagnostic_messages(&analyses[0]), Vec::<&str>::new());
    assert_eq!(
        diagnostic_messages(&analyses[1]),
        vec!["unresolved reference 'Widget'."]
    );
}

#[test]
fn unchanged_explicit_classpath_reuses_stubs_and_changed_contents_retry_fresh() {
    reset_java_stub_generations();
    let root = std::env::temp_dir().join(format!(
        "krusty-worker-java-stub-contents-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    write_payload_class(&root);
    let java =
        "package p; public class Widget { public q.Payload value() { return null; } }".to_string();
    let source = "fun use(w: p.Widget) = w.value()";
    let classpath = [root.clone()];
    let mut input = Vec::new();
    write_analysis_request(
        &mut input,
        source,
        std::slice::from_ref(&java),
        Some(&classpath),
    );
    write_analysis_request(
        &mut input,
        source,
        std::slice::from_ref(&java),
        Some(&classpath),
    );
    let mutate_after = input.len() as u64;
    write_analysis_request(
        &mut input,
        source,
        std::slice::from_ref(&java),
        Some(&classpath),
    );
    let class_file = root.join("q/Payload.class");
    let mut reader = MutateAfter {
        inner: Cursor::new(input),
        after: mutate_after,
        mutation: Some(Box::new(move || {
            std::fs::remove_file(class_file).expect("remove payload class");
        })),
    };
    let mut output = Vec::new();

    run_analysis_worker(&mut reader, &mut output, Vec::new()).unwrap();

    assert_eq!(java_stub_generations(), 1);
    let analyses = decode_worker_analyses(output, 2);
    assert_eq!(diagnostic_messages(&analyses[0]), Vec::<&str>::new());
    assert_eq!(diagnostic_messages(&analyses[1]), Vec::<&str>::new());

    let mut retry = Vec::new();
    write_analysis_request(&mut retry, source, &[java], Some(&classpath));
    let mut retry_output = Vec::new();
    run_analysis_worker(&mut Cursor::new(retry), &mut retry_output, Vec::new()).unwrap();

    assert_eq!(java_stub_generations(), 2);
    let analyses = decode_worker_analyses(retry_output, 1);
    assert_eq!(
        diagnostic_messages(&analyses[0]),
        vec![
            "Cannot access class 'q.Payload'. Check your module classpath for missing or \
             conflicting dependencies."
        ]
    );
    std::fs::remove_dir_all(root).expect("remove classpath directory");
}

fn write_payload_class(root: &Path) {
    let package = root.join("q");
    std::fs::create_dir_all(&package).expect("create payload package");
    let mut class = krusty::jvm::classfile::ClassWriter::new("q/Payload", "java/lang/Object");
    class.add_field(krusty::jvm::classfile::ACC_PUBLIC, "token", "I");
    std::fs::write(package.join("Payload.class"), class.finish()).expect("write payload class");
}
