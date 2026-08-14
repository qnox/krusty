use krusty::diag::DiagSink;
use krusty::frontend::{check_file, collect_signatures_with_cp};
use krusty::jvm::classpath::Classpath;
use krusty::jvm::ir_emit::{emit_all_with_opts, EmitOptions, EmitRun};
use std::rc::Rc;

#[test]
fn sealed_metadata_includes_sibling_file_subclasses() {
    let sources = [
        ("package p\nsealed class Root\n", "Base"),
        ("package p\nclass A : Root()\n", "A"),
        ("package p\nclass B : Root()\n", "B"),
    ];
    let mut diags = DiagSink::new();
    let files: Vec<_> = sources
        .iter()
        .map(|(source, _)| {
            let tokens = krusty::lexer::lex(source, &mut diags);
            krusty::parser::parse(source, &tokens, &mut diags)
        })
        .collect();
    assert!(!diags.has_errors());

    let cp = Rc::new(Classpath::new(Vec::new()));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let mut symbols = collect_signatures_with_cp(&files, platform, &mut diags);
    let info = check_file(&files[0], &mut symbols, &mut diags);
    assert!(!diags.has_errors());

    let runtime = krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone());
    let mut ir = krusty::ir_lower::lower_file(&files[0], &info, &symbols, &runtime)
        .expect("lower sealed base");
    krusty::jvm::backend::run_backend_passes(&mut ir, &files[0], "p/BaseKt", "main", &symbols)
        .expect("backend passes");
    let options = EmitOptions {
        emit_class_metadata: true,
        source_file: Some("Base.kt".to_string()),
        ..Default::default()
    };
    let classes = emit_all_with_opts(
        &ir,
        "p/BaseKt",
        &*cp,
        None,
        &options,
        &EmitRun::default(),
        &symbols,
    )
    .expect("emit sealed base");
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == "p/Root").then_some(bytes))
        .expect("Root class");
    assert!(!bytes
        .windows("PermittedSubclasses".len())
        .any(|window| window == b"PermittedSubclasses"));
    let class = krusty::jvm::classreader::parse_class(bytes).expect("parse Root class");
    let mut subclasses = krusty::jvm::metadata::class_sealed_subclasses(&class);
    subclasses.sort();
    assert_eq!(subclasses, ["p/A", "p/B"]);

    let java17_options = EmitOptions {
        class_major: Some(61),
        ..options
    };
    let java17_classes = emit_all_with_opts(
        &ir,
        "p/BaseKt",
        &*cp,
        None,
        &java17_options,
        &EmitRun::default(),
        &symbols,
    )
    .expect("emit Java 17 sealed base");
    let java17_root = java17_classes
        .iter()
        .find_map(|(name, bytes)| (name == "p/Root").then_some(bytes))
        .expect("Java 17 Root class");
    assert!(java17_root
        .windows("PermittedSubclasses".len())
        .any(|window| window == b"PermittedSubclasses"));
}
