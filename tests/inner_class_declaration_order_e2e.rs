//! kotlinc sorts a class's `InnerClasses` rows by each class's `fqNameWhenAvailable`, and an
//! anonymous object's name runs through the declaration kotlinc's IR moved its initializer into.
//!
//! A property initializer or `init` block of a class runs in the constructor, so an object there is
//! `Holder.<init>.<no name provided>`; an object's, a companion's or a top-level initializer runs in
//! the static initializer, `<clinit>`, and a companion's is its outer class's. Both sort ahead of a
//! named nested class, since `<` precedes every letter, which a table sorted by the classes' own
//! names alone gets backwards.
use super::common;
use krusty::jvm::classreader::parse_class;

const SHAPE: &str = "interface Shape { fun sides(): Int }\n";

/// Objects in a property initializer and an `init` block sort under `<init>`, ahead of the nested
/// class and of the member function's object.
#[test]
fn a_class_initializer_object_sorts_under_its_constructor() {
    let source = format!(
        "{SHAPE}class Holder {{\n\
         \x20   val second = object : Shape {{ override fun sides() = 2 }}\n\
         \x20   init {{ val first = object : Shape {{ override fun sides() = 1 }}; first.sides() }}\n\
         \x20   fun build(): Shape = object : Shape {{ override fun sides() = 3 }}\n\
         \x20   class Alpha\n\
         }}\n"
    );
    common::assert_same_inner_classes("ConstructorObjects", &source, &[], &["Holder"]);
}

/// An object declaration initializes its properties and runs its `init` blocks in `<clinit>`.
#[test]
fn an_object_initializer_object_sorts_under_its_static_initializer() {
    let source = format!(
        "{SHAPE}object Registry {{\n\
         \x20   val second = object : Shape {{ override fun sides() = 2 }}\n\
         \x20   init {{ val first = object : Shape {{ override fun sides() = 1 }}; first.sides() }}\n\
         \x20   class Alpha\n\
         }}\n"
    );
    common::assert_same_inner_classes("StaticObjects", &source, &[], &["Registry"]);
}

/// A companion's property initializer runs in its outer class's `<clinit>`, which sorts it ahead of
/// the outer class's own `<init>` objects.
#[test]
fn a_companion_initializer_object_sorts_under_the_outer_static_initializer() {
    let source = format!(
        "{SHAPE}class Outer {{\n\
         \x20   companion object {{\n\
         \x20       val second = object : Shape {{ override fun sides() = 2 }}\n\
         \x20   }}\n\
         \x20   init {{ val first = object : Shape {{ override fun sides() = 1 }}; first.sides() }}\n\
         \x20   class Alpha\n\
         }}\n"
    );
    common::assert_same_inner_classes(
        "CompanionObjects",
        &source,
        &[],
        &["Outer", "Outer$Companion"],
    );
}

/// An interface companion keeps its static state, so its initializer runs in its own `<clinit>` and
/// sorts after the companion's own row.
#[test]
fn an_interface_companion_initializer_object_sorts_under_its_own_static_initializer() {
    let source = format!(
        "{SHAPE}interface Registry {{\n\
         \x20   companion object {{\n\
         \x20       val shared = object : Shape {{ override fun sides() = 2 }}\n\
         \x20   }}\n\
         }}\n"
    );
    common::assert_same_inner_classes(
        "InterfaceCompanionObjects",
        &source,
        &[],
        &["Registry$Companion"],
    );
}

/// A suspend function's continuation class is declared in that function: `Task.run.<no name
/// provided>` sorts after the nested classes, where its internal name `Task$run$1` sorted first.
#[test]
fn a_continuation_class_sorts_under_the_function_it_drives() {
    let source = format!(
        "{SHAPE}class Task {{\n\
         \x20   val first = object : Shape {{ override fun sides() = 1 }}\n\
         \x20   class Alpha\n\
         \x20   class Zeta\n\
         \x20   suspend fun run(): Int {{ pause(); return 1 }}\n\
         \x20   suspend fun pause() {{}}\n\
         \x20   fun make() {{ Alpha(); Zeta() }}\n\
         }}\n"
    );
    common::assert_same_inner_classes("ContinuationClasses", &source, &[], &["Task"]);
}

/// An enum entry with a body compiles to a subclass nested in its enum, so the subclass lists its
/// own `InnerClasses` row, as any nested class does.
#[test]
fn an_enum_entry_subclass_lists_itself_as_a_nested_class() {
    let source = "enum class Mode {\n\
                  \x20   FIRST { override fun label() = \"first\" },\n\
                  \x20   SECOND;\n\
                  \x20   open fun label() = \"other\"\n\
                  }\n";
    common::assert_same_inner_classes("EnumEntryBodies", source, &[], &["Mode", "Mode$FIRST"]);
}

/// kotlinc generates a local class from the class whose code declares it and lists it there, even
/// when that code never names it: `Unused` in a member function, `Top` in the facade and `Deep` in
/// an inner class's function all get a row in their declaring class.
#[test]
fn a_class_lists_every_local_class_its_code_declares() {
    let source = "class Holder {\n\
                  \x20   fun make(): Any {\n\
                  \x20       class Unused\n\
                  \x20       open class Base { fun v() = 1 }\n\
                  \x20       class Leaf : Base()\n\
                  \x20       return Leaf()\n\
                  \x20   }\n\
                  \x20   inner class In { fun g(): Int { class Deep; return 1 } }\n\
                  }\n\
                  fun top(): Int { class Top; return 3 }\n";
    common::assert_same_inner_classes(
        "DeclaredLocalClasses",
        source,
        &[],
        &["Holder", "Holder$In", "DeclaredLocalClassesKt"],
    );
}

/// An inline function's anonymous object is declared by the file that declares the function. A
/// caller in another file of the module inlines it and names it in no signature of its own, so its
/// facade lists no row for it, while the function's own facade does.
#[test]
fn a_caller_lists_no_local_class_of_an_inline_function_in_another_file() {
    let declarations = "interface Task { fun run() }\n\
                        inline fun wrap(crossinline block: () -> Unit) = object : Task {\n\
                        \x20   override fun run() { block() }\n\
                        }\n";
    let caller = "class Box { val task = wrap { } }\n\
                  fun callerMarker(): Int = 1\n";
    let sources = [("Declarations.kt", declarations), ("Caller.kt", caller)];
    for class in ["DeclarationsKt", "CallerKt"] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        let rows = |bytes: &[u8]| parse_class(bytes).expect("a parseable class").inner_classes;
        assert_eq!(
            rows(&pair.krusty),
            rows(&pair.kotlinc),
            "{class}'s InnerClasses rows"
        );
    }
}

/// A foreign inline property's object belongs to the file declaring its getter, just like one from
/// a foreign inline function. The caller's otherwise unrelated facade must not gain that local
/// class merely because the accessor body remains in its IR as an inline template.
#[test]
fn a_caller_lists_no_local_class_of_an_inline_property_in_another_file() {
    let declarations = "interface Task { fun run() }\n\
                        inline val wrapped: Task\n\
                        \x20   get() = object : Task { override fun run() {} }\n";
    let caller = "class Box { val task = wrapped }\n\
                  fun callerMarker(): Int = 1\n";
    let sources = [("Declarations.kt", declarations), ("Caller.kt", caller)];
    for class in ["DeclarationsKt", "CallerKt"] {
        let pair = common::ModuleClassPair::compile(&sources, class);
        let rows = |bytes: &[u8]| parse_class(bytes).expect("a parseable class").inner_classes;
        assert_eq!(
            rows(&pair.krusty),
            rows(&pair.kotlinc),
            "{class}'s InnerClasses rows"
        );
    }
}
