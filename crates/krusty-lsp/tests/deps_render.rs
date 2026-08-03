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

fn jdk_and_stdlib_classpath() -> Option<Rc<Classpath>> {
    let modules = krusty::toolchain::jdk_modules()?;
    let stdlib = krusty::toolchain::stdlib_jar()?;
    let cp = Rc::new(Classpath::new(vec![modules, stdlib]));
    (!cp.scan_types().is_empty()).then_some(cp)
}

#[test]
fn renders_a_builtin_with_its_kotlin_supertypes_and_members() {
    let Some(cp) = jdk_and_stdlib_classpath() else {
        return;
    };

    let rendered =
        match materialize(&cp, "kotlin/Long", "", "", false).expect("materialize kotlin/Long") {
            MaterializedSource::Rendered(rendered) => rendered,
            MaterializedSource::Attached { .. } => panic!("expected a rendered stub"),
        };

    assert!(
        rendered.text.contains("class Long : Number, Comparable {"),
        "wrong supertypes:\n{}",
        &rendered.text[..rendered.text.len().min(400)]
    );
    assert!(
        rendered.text.contains("fun plus(p0: Long): Long"),
        "builtin members missing:\n{}",
        &rendered.text[..rendered.text.len().min(400)]
    );
}

#[test]
fn qualifies_distinct_supertypes_with_the_same_simple_name() {
    let Some((mut lib, _)) = load("kotlin/collections/AbstractList") else {
        return;
    };
    lib.supertypes = vec![
        "java/lang/Number".to_string(),
        "kotlin/Number".to_string(),
        "alpha/Foo".to_string(),
        "beta/Foo".to_string(),
    ]
    .into();

    let rendered = render_library_class("example/Subject", &lib, None);

    assert!(
        rendered.text.contains(" : Number, alpha.Foo, beta.Foo {"),
        "{}",
        rendered.text
    );
}

#[test]
fn opens_the_real_stdlib_source_for_a_builtin() {
    let Some(cp) = jdk_and_stdlib_classpath() else {
        return;
    };
    let stdlib = krusty::toolchain::stdlib_jar().expect("stdlib jar");
    if !stdlib.with_file_name("kotlin-stdlib-sources.jar").is_file() {
        return;
    }

    let materialized =
        materialize(&cp, "kotlin/Long", "", "", true).expect("materialize kotlin/Long");
    assert!(
        matches!(materialized, MaterializedSource::Attached { .. }),
        "fell back to a rendered stub instead of the stdlib source"
    );
    let (text, span) = materialized.into_text_and_span("", "");

    assert!(
        text.contains("actual class Long"),
        "not the JVM stdlib source:\n{}",
        &text[..text.len().min(400)]
    );
    assert_eq!(&text[span.lo as usize..span.hi as usize], "Long");
    assert!(
        text[..span.lo as usize].trim_end().ends_with("class"),
        "span is not on the declaration: {:?}",
        &text[span.lo.saturating_sub(60) as usize..span.hi as usize]
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

    let src = attached_source(&classes, "com/example/Widget", "");
    assert!(
        src.as_ref()
            .is_some_and(|(text, _)| text.contains("class Widget")),
        "attached source not read"
    );
    assert!(attached_source(&classes, "com/example/Missing", "").is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn reads_a_default_package_source_from_the_archive_root() {
    let dir = std::env::temp_dir().join(format!("krusty-default-package-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("Widget.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[("Widget.kt", b"class Widget")],
    );

    let (text, span) = attached_source(&classes, "Widget", "").expect("default-package source");

    assert_eq!(text, "class Widget");
    assert_eq!(&text[span.lo as usize..span.hi as usize], "Widget");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rejects_a_same_named_source_from_the_wrong_package() {
    let dir = std::env::temp_dir().join(format!("krusty-package-match-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[
            (
                "com/example/Widget.kt",
                b"package wrong\nclass Widget { val wrong = true }",
            ),
            (
                "generated/Actual.kt",
                b"package com.example\nclass Widget { val correct = true }",
            ),
        ],
    );

    let (text, _) =
        attached_source(&classes, "com/example/Widget", "").expect("matching package source");

    assert!(text.contains("correct"));
    assert!(!text.contains("wrong = true"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finds_the_sources_jar_gradle_stores_in_a_sibling_hash_directory() {
    let dir = std::env::temp_dir().join(format!("krusty-gradle-cache-{}", std::process::id()));
    let classes_dir = dir.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let sources_dir = dir.join("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    std::fs::create_dir_all(&classes_dir).unwrap();
    std::fs::create_dir_all(&sources_dir).unwrap();
    let classes = classes_dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &sources_dir.join("widget-1.0-sources.jar"),
        &[(
            "com/example/Widget.kt",
            b"package com.example\nclass Widget",
        )],
    );

    let src = attached_source(&classes, "com/example/Widget", "");
    assert!(
        src.as_ref()
            .is_some_and(|(text, _)| text.contains("class Widget")),
        "sources jar in a sibling hash directory not found"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finds_a_declaration_in_a_source_file_named_after_neither_package_nor_class() {
    let dir = std::env::temp_dir().join(format!("krusty-multiclass-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "commonMain/com/example/Shapes.kt",
            b"package com.example\n\nclass Gadget\n\nclass Widget\n",
        )],
    );

    let src = attached_source(&classes, "com/example/Widget", "");
    assert!(
        src.as_ref()
            .is_some_and(|(text, _)| text.contains("class Widget")),
        "declaration not found by scanning the package's sources"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finds_a_top_level_function_in_the_facade_source() {
    let dir = std::env::temp_dir().join(format!("krusty-facade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/UtilsKt.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "commonMain/com/example/Helpers.kt",
            b"package com.example\n\nfun makeGadget() = Unit\n\nfun makeWidget() = Unit\n",
        )],
    );
    let cp = Rc::new(Classpath::new(vec![classes]));

    let (text, span) = materialize(&cp, "com/example/UtilsKt", "makeWidget", "()V", true)
        .expect("materialize facade")
        .into_text_and_span("makeWidget", "");

    assert!(
        text.contains("fun makeWidget"),
        "not the facade's source:\n{text}"
    );
    let declaration = text.find("fun makeWidget").expect("declaration present") + "fun ".len();
    assert_eq!(
        span.lo as usize,
        declaration,
        "span points at {:?}, not the function declaration",
        &text[span.lo as usize..span.hi as usize]
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finds_generic_extension_and_overloaded_facade_callables() {
    let dir = std::env::temp_dir().join(format!("krusty-facade-signatures-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/UtilsKt.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "commonMain/com/example/Helpers.kt",
            b"package com.example\n\
              fun choose(value: Int) = Unit\n\
              fun <T> choose(left: T, right: T) = Unit\n\
              fun String.decorate(value: Int) = Unit\n\
              val String.label: String get() = this\n",
        )],
    );
    let cp = Rc::new(Classpath::new(vec![classes]));

    let (text, overloaded) = materialize(
        &cp,
        "com/example/UtilsKt",
        "choose",
        "(Ljava/lang/Object;Ljava/lang/Object;)V",
        true,
    )
    .expect("materialize overloaded function")
    .into_text_and_span("choose", "");
    let (extension_text, extension) = materialize(
        &cp,
        "com/example/UtilsKt",
        "decorate",
        "(Ljava/lang/String;I)V",
        true,
    )
    .expect("materialize extension function")
    .into_text_and_span("decorate", "");
    let (_, property) =
        attached_source(&dir.join("widget-1.0.jar"), "com/example/UtilsKt", "label")
            .expect("extension property");

    let second_choose = text.rfind("choose").expect("second overload");
    assert_eq!(overloaded.lo as usize, second_choose);
    assert_eq!(
        &extension_text[extension.lo as usize..extension.hi as usize],
        "decorate"
    );
    assert_eq!(
        &extension_text[property.lo as usize..property.hi as usize],
        "label"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn resolves_nested_types_and_members_inside_the_selected_type() {
    let dir = std::env::temp_dir().join(format!("krusty-nested-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(
        &classes,
        &[
            ("com/example/Outer.class", b""),
            ("com/example/Outer$Inner.class", b""),
            ("com/example/Outer$Companion.class", b""),
        ],
    );
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "com/example/Outer.kt",
            b"package com.example\n\
              class Outer {\n\
                  fun target() = Unit\n\
                  companion object\n\
                  class Inner {\n\
                      fun earlier() = target()\n\
                      fun target() = Unit\n\
                  }\n\
              }\n",
        )],
    );

    let (text, nested_span) =
        attached_source(&classes, "com/example/Outer$Inner", "").expect("nested source");
    let (_, member_span) = attached_source(&classes, "com/example/Outer$Inner", "target")
        .expect("nested member source");
    let (_, companion_span) =
        attached_source(&classes, "com/example/Outer$Companion", "").expect("companion source");

    assert_eq!(
        &text[nested_span.lo as usize..nested_span.hi as usize],
        "Inner"
    );
    let nested_target = text.rfind("fun target").expect("nested target") + "fun ".len();
    assert_eq!(member_span.lo as usize, nested_target);
    assert_eq!(
        &text[companion_span.lo as usize..companion_span.hi as usize],
        "companion"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn points_at_the_declaration_rather_than_an_earlier_mention() {
    let dir = std::env::temp_dir().join(format!("krusty-decl-span-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let classes = dir.join("widget-1.0.jar");
    write_jar(&classes, &[("com/example/Widget.class", b"")]);
    write_jar(
        &dir.join("widget-1.0-sources.jar"),
        &[(
            "com/example/Shapes.kt",
            b"package com.example\n\n// Widget helpers live here.\nfun makeWidget(): Widget = TODO()\n\nclass Widget\n",
        )],
    );
    let cp = Rc::new(Classpath::new(vec![classes]));

    let (text, span) = materialize(&cp, "com/example/Widget", "", "", true)
        .expect("materialize widget")
        .into_text_and_span("", "");

    let declaration = text.find("class Widget").expect("declaration present") + "class ".len();
    assert_eq!(
        span.lo as usize,
        declaration,
        "span points at {:?}, not the declaration",
        &text[span.lo as usize..span.hi as usize]
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn materialize_prefers_attached_source_and_honors_the_flag() {
    let cp = Rc::new(stdlib_classpath());
    if cp.scan_types().is_empty() {
        return;
    }

    match materialize(&cp, "kotlin/text/Regex", "", "", true).expect("materialize Regex") {
        MaterializedSource::Rendered(rendered) => {
            assert!(rendered.text.contains("class Regex"), "{}", rendered.text)
        }
        MaterializedSource::Attached { text, .. } => assert!(text.contains("Regex")),
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
    match materialize(&cp2, "com/example/Widget", "", "", true).expect("materialize widget") {
        MaterializedSource::Attached { text, .. } => assert!(text.contains("class Widget")),
        MaterializedSource::Rendered(_) => panic!("expected attached source, got a stub"),
    }
    assert!(materialize(&cp2, "com/example/Widget", "", "", false).is_none());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_real_stdlib_classpath_is_searchable_by_class_name() {
    let cp = Rc::new(stdlib_classpath());
    if cp.scan_types().is_empty() {
        return;
    }
    let tree = cp.package_tree();
    let index = krusty_lsp::DependencySymbolIndex::from_internal_names(
        tree.classes().map(|(internal, _)| internal),
    );

    assert!(
        index.class_count() > 100,
        "the stdlib declares more than a handful of classes, got {}",
        index.class_count()
    );
    assert!(index.is_complete());

    let found = index.candidates("AbstractList", 8);
    let listed = found
        .iter()
        .find(|candidate| candidate.internal == "kotlin/collections/AbstractList")
        .expect("kotlin.collections.AbstractList is on the stdlib classpath");
    assert_eq!(listed.name, "AbstractList");
    assert_eq!(listed.package, "kotlin.collections");

    // Every candidate must name a class the classpath can actually resolve, or the location step
    // would have nothing to render.
    for candidate in index.candidates("Iterable", 8) {
        assert!(
            cp.find(&candidate.internal).is_some(),
            "{} was indexed but the classpath cannot find it",
            candidate.internal
        );
    }
}

#[test]
fn a_ranked_dependency_class_becomes_a_file_with_a_range() {
    let cp = Rc::new(stdlib_classpath());
    if cp.scan_types().is_empty() {
        return;
    }
    let index = krusty_lsp::DependencySymbolIndex::from_classpath(&cp);
    let cache = std::env::temp_dir().join(format!(
        "krusty-dep-locate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let candidates = index.candidates("AbstractList", 4);
    assert!(!candidates.is_empty());
    let located = krusty_lsp::locate_dependencies(&cp, &cache, candidates, false);

    let listed = located
        .iter()
        .find(|found| found.candidate.internal == "kotlin/collections/AbstractList")
        .expect("the ranked class must be locatable");
    // A client that will not open a URI without a range needs both, and the file has to be on disk
    // by the time the response leaves.
    assert!(
        listed.path.is_file(),
        "{} was not written",
        listed.path.display()
    );
    let text = std::fs::read_to_string(&listed.path).unwrap();
    assert_eq!(text, listed.text);
    assert_eq!(
        &text[listed.span.lo as usize..listed.span.hi as usize],
        "AbstractList"
    );

    std::fs::remove_dir_all(&cache).ok();
}
