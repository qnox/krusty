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
    inline fun read(): Any = Holder.item\n\
    inline fun cast(value: Any): Any = value as Holder.Item\n";

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

/// A `checkcast` the inline body copies names the class as a class constant, still without a row.
#[test]
fn a_class_constant_only_an_inline_body_names_gives_no_row() {
    let source = format!("{HOLDER}class Caster {{ fun use(value: Any): Any = cast(value) }}\n");
    common::assert_same_inner_classes("InlineCast", &source, &[], &["Caster"]);
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

/// A suspend function's body is rewritten by the coroutine transformer after it is generated. The
/// rewrite re-encodes the copied read as kotlinc's transformer does, with raw ASM, so `Machine`
/// still lists no `Holder$Item`, and neither does the transformer's own `checkcast` restoring the
/// spilled value. `OwnMachine`'s own read keeps the row, and both list their continuation class.
#[test]
fn a_transformed_suspend_body_keeps_copied_references_unmapped() {
    let source = format!(
        "{HOLDER}suspend fun pause() {{}}\n\
         class Machine {{ suspend fun use(): Any {{ val item = read(); pause(); return item }} }}\n\
         class OwnMachine {{ suspend fun use(): Any {{ val item = Holder.item; pause(); return item }} }}\n"
    );
    common::assert_same_inner_classes(
        "TransformedInline",
        &source,
        &[common::stdlib_jar()],
        &["Machine", "OwnMachine"],
    );
}

/// A copied call names its nested owner only as the member reference's class: `Counter`'s
/// expansion of `bump` lists no `Tally$Counter`, while `OwnCounter`'s own call keeps the row.
#[test]
fn a_copied_call_on_a_nested_owner_gives_no_row() {
    let source = "object Tally {\n\
                  \x20   object Counter { @JvmStatic fun next(): Int = 1 }\n\
                  }\n\
                  inline fun bump(): Int = Tally.Counter.next()\n\
                  class Counter { fun use(): Int = bump() }\n\
                  class OwnCounter { fun use(): Int = Tally.Counter.next() }\n";
    common::assert_same_inner_classes("InlineOwner", source, &[], &["Counter", "OwnCounter"]);
}
