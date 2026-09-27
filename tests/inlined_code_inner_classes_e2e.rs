//! kotlinc lists a nested class in a class's `InnerClasses` when its type mapper maps a signature
//! naming it while generating that class. An inline function's body is compiled once and its
//! instructions are copied into each call site, so a member reference that only the copied body
//! names never passes through the caller's mapper: `Holder.item`'s `LHolder$Item;` gives the
//! caller no `Holder$Item` row. The same holds whether the inline function is in this module or
//! on the classpath, while a reference the caller's own code makes keeps the row.
use super::common;
use krusty::jvm::classreader::{parse_class, InnerClassRef};
use std::path::PathBuf;

const HOLDER: &str = "object Holder {\n\
    \x20   class Item\n\
    \x20   @JvmField val item = Item()\n\
    }\n\
    inline fun read(): Any = Holder.item\n";

/// Compile `source` with kotlinc and krusty against `classpath` and require each of `classes` to
/// carry kotlinc's `InnerClasses` rows, in kotlinc's order.
fn assert_same_inner_classes(stem: &str, source: &str, classpath: &[PathBuf], classes: &[&str]) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("fixture source");
    let mut args = vec!["-d".to_string(), reference.to_string_lossy().into_owned()];
    if !classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(
            std::env::join_paths(classpath)
                .expect("a joinable classpath")
                .to_string_lossy()
                .into_owned(),
        );
    }
    args.push(source_path.to_string_lossy().into_owned());
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let mut krusty_classpath = classpath.to_vec();
    krusty_classpath.push(common::stdlib_jar());
    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(source, stem, &krusty_classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{stem}: krusty rejected the fixture: {:?}",
                common::front_end_diagnostics(source, &krusty_classpath, Some(jdk.as_path()))
            )
        });
    let rows = |bytes: &[u8]| -> Vec<InnerClassRef> {
        parse_class(bytes).expect("a parseable class").inner_classes
    };
    for class in classes {
        let expected = std::fs::read(reference.join(format!("{class}.class")))
            .unwrap_or_else(|_| panic!("{stem}: kotlinc emits {class}"));
        let (_, actual) = compiled
            .iter()
            .find(|(name, _)| name == class)
            .unwrap_or_else(|| panic!("{stem}: krusty did not emit {class}"));
        assert_eq!(
            rows(actual),
            rows(&expected),
            "{stem}: {class}'s InnerClasses rows"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `User` reads `Holder.item` only through the expanded body of a same-module inline function, so
/// it lists no `Holder$Item`; `Direct` reads it itself and keeps the row.
#[test]
fn a_same_module_inline_body_names_no_nested_class_for_its_caller() {
    let source = format!(
        "{HOLDER}class User {{ fun use(): Any = read() }}\n\
         class Direct {{ fun use(): Any = Holder.item }}\n"
    );
    assert_same_inner_classes("SameModuleInline", &source, &[], &["User", "Direct"]);
}

/// The same body compiled into a library and inlined from the classpath gives the caller no row
/// either, while the caller's own read beside it still does.
#[test]
fn a_classpath_inline_body_names_no_nested_class_for_its_caller() {
    let library = common::kotlinc_library(&format!("package icfixture\n{HOLDER}"))
        .expect("reference compiler must build the inline fixture");
    let source = "import icfixture.Holder\n\
                  import icfixture.read\n\
                  class User { fun use(): Any = read() }\n\
                  class Both { fun copied(): Any = read()\n fun own(): Any = Holder.item }\n";
    assert_same_inner_classes("ClasspathInline", source, &[library], &["User", "Both"]);
}
