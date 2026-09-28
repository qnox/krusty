//! kotlinc lists a nested class in a class's `InnerClasses` when its type mapper maps a signature
//! naming it while generating that class. An inline function's body is compiled once and its
//! instructions are copied into each call site, so a member reference that only the copied body
//! names never passes through the caller's mapper: `Holder.item`'s `LHolder$Item;` gives the
//! caller no `Holder$Item` row. The same holds whether the inline function is in this module or
//! on the classpath, while a reference the caller's own code makes keeps the row.
use super::common;

const HOLDER: &str = "object Holder {\n\
    \x20   class Item\n\
    \x20   @JvmField val item = Item()\n\
    }\n\
    inline fun read(): Any = Holder.item\n";

/// `User` reads `Holder.item` only through the expanded body of a same-module inline function, so
/// it lists no `Holder$Item`; `Direct` reads it itself and keeps the row.
#[test]
fn a_same_module_inline_body_names_no_nested_class_for_its_caller() {
    let source = format!(
        "{HOLDER}class User {{ fun use(): Any = read() }}\n\
         class Direct {{ fun use(): Any = Holder.item }}\n"
    );
    common::assert_same_inner_classes("SameModuleInline", &source, &[], &["User", "Direct"]);
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
    common::assert_same_inner_classes("ClasspathInline", source, &[library], &["User", "Both"]);
}
