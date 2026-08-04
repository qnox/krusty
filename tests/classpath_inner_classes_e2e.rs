use super::common;
use krusty::jvm::classreader::parse_class;

#[test]
fn copies_referenced_classpath_inner_class_metadata() {
    let Some(lib) = common::compile_lib(
        "inner_classes",
        "package dep\nclass Outer { class Nested }\n",
    ) else {
        return;
    };
    let expected = std::fs::read(lib.join("dep/Outer$Nested.class"))
        .ok()
        .and_then(|bytes| parse_class(&bytes).ok())
        .and_then(|class| {
            class
                .inner_classes
                .into_iter()
                .find(|entry| entry.inner == "dep/Outer$Nested")
        })
        .expect("dependency self entry");
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process(
        "package app\nfun nested(value: Any) = value is dep.Outer.Nested\n",
        "Use",
        &[lib, stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/UseKt")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("emitted facade");

    assert!(emitted.inner_classes.contains(&expected));
}
