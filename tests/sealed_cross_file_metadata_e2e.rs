use super::common;

#[test]
fn sealed_metadata_includes_sibling_file_subclasses() {
    let sources = [
        ("package p\nsealed class Root\n", "Base"),
        ("package p\nclass A : Root()\n", "A"),
        ("package p\nclass B : Root()\n", "B"),
    ];
    let inputs = [
        ("Base.kt", sources[0].0),
        ("A.kt", sources[1].0),
        ("B.kt", sources[2].0),
    ];
    let classes = common::compile_in_process_files_target(&inputs, &[], None, None)
        .expect("production compiler emits sealed source set");
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

    let java17_classes = common::compile_in_process_files_target(&inputs, &[], None, Some(61))
        .expect("production compiler emits Java 17 sealed source set");
    let java17_root = java17_classes
        .iter()
        .find_map(|(name, bytes)| (name == "p/Root").then_some(bytes))
        .expect("Java 17 Root class");
    assert!(java17_root
        .windows("PermittedSubclasses".len())
        .any(|window| window == b"PermittedSubclasses"));
}
