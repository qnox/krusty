use krusty::diag::DiagSink;
use krusty::jvm::classpath::Classpath;
use krusty::source::SourceInput;
use std::rc::Rc;

fn compile_sources(sources: &[(&str, &str)], class_major: Option<u16>) -> Vec<(String, Vec<u8>)> {
    let mut diags = DiagSink::new();
    let inputs = sources
        .iter()
        .map(|(source, stem)| SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(_, stem)| (*stem).to_string())
        .collect::<Vec<_>>();
    let cp = Rc::new(Classpath::new(Vec::new()));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp.clone()));
    let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
        &inputs,
        platform,
        &krusty::features::LangFeatures::default(),
        |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
        &mut diags,
    );
    let outputs = krusty::compiler::emit_analyzed(
        analysis,
        &stems,
        &krusty::jvm::JvmBackend::new(cp).with_class_major(class_major),
        "main",
        &mut diags,
    );
    assert!(diags.diags.is_empty(), "{:?}", diags.diags);
    outputs
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_suffix(".class")
                .map(|name| (name.to_string(), bytes))
        })
        .collect()
}

#[test]
fn sealed_metadata_includes_sibling_file_subclasses() {
    let sources = [
        ("package p\nsealed class Root\n", "Base"),
        ("package p\nclass A : Root()\n", "A"),
        ("package p\nclass B : Root()\n", "B"),
    ];
    let classes = compile_sources(&sources, None);
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

    let java17_classes = compile_sources(&sources, Some(61));
    let java17_root = java17_classes
        .iter()
        .find_map(|(name, bytes)| (name == "p/Root").then_some(bytes))
        .expect("Java 17 Root class");
    assert!(java17_root
        .windows("PermittedSubclasses".len())
        .any(|window| window == b"PermittedSubclasses"));
}
