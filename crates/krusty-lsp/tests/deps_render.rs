use std::rc::Rc;

use krusty::jvm::classpath::Classpath;
use krusty::jvm::jvm_libraries::JvmLibraries;
use krusty::jvm::metadata::KotlinMeta;
use krusty::libraries::LibraryType;
use krusty::symbol_source::SymbolSource;
use krusty::toolchain::stdlib_classpath;
use krusty_lsp::deps_render::{
    attached_source, materialize, render_library_class, MaterializedSource,
};

fn load(internal: &str) -> Option<(LibraryType, Option<KotlinMeta>)> {
    let cp = Rc::new(stdlib_classpath());
    if cp.scan_types().is_empty() {
        return None;
    }
    let meta = cp.find(internal).map(|ci| ci.meta.clone());
    let libs = JvmLibraries::new(cp);
    let lib = libs.resolve_type(internal)?;
    Some((lib, meta))
}

#[test]
fn renders_class_header_with_package_and_name_span() {
    let Some((lib, meta)) = load("kotlin/collections/AbstractList") else {
        return;
    };

    let out = render_library_class("kotlin/collections/AbstractList", &lib, meta.as_ref());

    assert!(
        out.text.starts_with("package kotlin.collections"),
        "got:\n{}",
        out.text
    );
    assert!(
        out.text.contains("class AbstractList"),
        "got:\n{}",
        out.text
    );
    assert_eq!(
        &out.text[out.type_span.lo as usize..out.type_span.hi as usize],
        "AbstractList"
    );
}

#[test]
fn renders_member_functions_with_accurate_spans() {
    let Some((lib, meta)) = load("kotlin/text/Regex") else {
        return;
    };

    let out = render_library_class("kotlin/text/Regex", &lib, meta.as_ref());

    assert!(
        out.text.contains("fun "),
        "no functions rendered:\n{}",
        &out.text[..out.text.len().min(800)]
    );
    assert!(!out.members.is_empty(), "no member spans recorded");
    assert!(
        out.members.iter().any(|(_, s)| out
            .text
            .contains(&format!("fun {}", &out.text[s.lo as usize..s.hi as usize]))),
        "no function member span found"
    );
    for (key, span) in &out.members {
        let slice = &out.text[span.lo as usize..span.hi as usize];
        assert!(!slice.is_empty(), "empty span for member {}", key.name);
    }
}

#[test]
fn renders_function_type_parameters() {
    let Some((lib, meta)) = load("kotlin/collections/AbstractCollection") else {
        return;
    };

    let out = render_library_class("kotlin/collections/AbstractCollection", &lib, meta.as_ref());

    assert!(
        out.text.contains("fun <T> toArray"),
        "function type parameters not rendered:\n{}",
        &out.text[..out.text.len().min(600)]
    );
}

#[test]
fn renders_val_and_var_properties() {
    let Some((lib, meta)) = load("kotlin/text/Regex") else {
        return;
    };

    let out = render_library_class("kotlin/text/Regex", &lib, meta.as_ref());

    assert!(
        out.text.contains("val "),
        "no properties rendered:\n{}",
        &out.text[..out.text.len().min(1000)]
    );
    assert!(out.text.contains("val pattern"), "pattern property missing");
}

#[test]
fn renders_enum_entries() {
    let Some((lib, meta)) = load("kotlin/text/RegexOption") else {
        return;
    };

    let out = render_library_class("kotlin/text/RegexOption", &lib, meta.as_ref());

    assert!(
        out.text.contains("enum class RegexOption"),
        "not rendered as an enum:\n{}",
        &out.text[..out.text.len().min(300)]
    );
    assert!(
        out.text.contains("IGNORE_CASE"),
        "enum entry missing:\n{}",
        out.text
    );
    assert!(
        out.members.iter().any(|(k, _)| k.name == "IGNORE_CASE"),
        "IGNORE_CASE not a member"
    );
}

fn write_jar(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    use std::io::Write;
    let file = std::fs::File::create(path).expect("create jar");
    let mut zip = zip::ZipWriter::new(file);
    for (name, data) in entries {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("start entry");
        zip.write_all(data).expect("write entry");
    }
    zip.finish().expect("finish jar");
}

#[test]
fn reads_attached_kotlin_source_when_sources_jar_present() {
    let dir = std::env::temp_dir().join(format!("krusty-a7-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    let sources = dir.join("widget-1.0-sources.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &sources,
        &[(
            "com/example/Widget.kt",
            b"package com.example\nclass Widget",
        )],
    );

    let src = attached_source(&classes, "com/example/Widget");
    assert!(
        src.as_deref().is_some_and(|s| s.contains("class Widget")),
        "attached source not read: {src:?}"
    );
    assert!(attached_source(&classes, "com/example/Missing").is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn materialize_prefers_attached_source_and_honors_the_flag() {
    let cp = Rc::new(stdlib_classpath());
    if cp.scan_types().is_empty() {
        return;
    }

    match materialize(&cp, "kotlin/text/Regex", true).expect("materialize Regex") {
        MaterializedSource::Rendered(rendered) => {
            assert!(rendered.text.contains("class Regex"), "{}", rendered.text)
        }
        MaterializedSource::Attached { text } => assert!(text.contains("Regex")),
    }

    let dir = std::env::temp_dir().join(format!("krusty-a8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "com/example/Widget.kt",
            b"package com.example\nclass Widget",
        )],
    );
    let cp2 = Rc::new(Classpath::new(vec![classes]));
    match materialize(&cp2, "com/example/Widget", true).expect("materialize widget") {
        MaterializedSource::Attached { text } => assert!(text.contains("class Widget")),
        MaterializedSource::Rendered(_) => panic!("expected attached source, got a stub"),
    }
    assert!(materialize(&cp2, "com/example/Widget", false).is_none());

    std::fs::remove_dir_all(&dir).ok();
}
