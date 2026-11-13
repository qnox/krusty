use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use std::rc::Rc;

#[test]
fn sealed_metadata_includes_sibling_file_subclasses() {
    let sources = [
        ("package p\nsealed class Root\n", "Base"),
        ("package p\nclass A : Root()\n", "A"),
        ("package p\nclass B : Root()\n", "B"),
    ];
    let cp = Rc::new(Classpath::new(Vec::new()));
    let compile = |class_major| {
        let mut diagnostics = krusty::diag::DiagSink::new();
        let stems = ["Base".to_string(), "A".to_string(), "B".to_string()];
        let inputs = sources
            .iter()
            .zip(&stems)
            .map(|((source, _), stem)| {
                krusty::source::SourceInput::kotlin(source).with_file_stem(stem)
            })
            .collect::<Vec<_>>();
        let analysis = krusty::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            Box::new(JvmLibraries::new(cp.clone())),
            &krusty::features::LangFeatures::new(),
            |files, symbols| krusty::jvm::prepare_module_symbols(files, &stems, symbols),
            &mut diagnostics,
        );
        let backend = krusty::jvm::JvmBackend::new(cp.clone()).with_class_major(class_major);
        let artifacts =
            krusty::compiler::emit_analyzed(analysis, &stems, &backend, "main", &mut diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diags);
        artifacts
            .into_iter()
            .filter_map(|(path, bytes)| {
                path.strip_suffix(".class")
                    .map(|name| (name.to_string(), bytes))
            })
            .collect::<Vec<_>>()
    };
    let classes = compile(None);
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

    let java17_classes = compile(Some(61));
    let java17_root = java17_classes
        .iter()
        .find_map(|(name, bytes)| (name == "p/Root").then_some(bytes))
        .expect("Java 17 Root class");
    assert!(java17_root
        .windows("PermittedSubclasses".len())
        .any(|window| window == b"PermittedSubclasses"));
}
