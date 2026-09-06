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
    let names: Vec<&String> = classes.iter().map(|(name, _)| name).collect();
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/UseKt")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .unwrap_or_else(|| panic!("emitted facade; classes: {names:?}"));

    assert!(emitted.inner_classes.contains(&expected));
}

#[test]
fn inherited_inner_constructor_metadata_excludes_its_enclosing_instance() {
    let Some(lib) = common::compile_lib(
        "inherited_inner_constructor",
        "open class Foo(val z: Int) {\n\
         \x20 open inner class FooInner { fun foo(): Int = z }\n\
         }\n",
    ) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let source = "class Bar : Foo(42) {\n\
        \x20 inner class BarInner(val x: Int) : FooInner()\n\
        }\n\
        fun box(): String {\n\
        \x20 val value = Bar().BarInner(117)\n\
        \x20 return if (value.x == 117 && value.foo() == 42) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(
        common::front_end_diagnostics(
            source,
            &[lib, stdlib],
            Some(common::jdk_modules().as_path()),
        ),
        Vec::<String>::new(),
    );
}

#[test]
fn classpath_inner_constructor_is_visible_and_bound_through_an_extension_receiver() {
    let Some(lib) = common::compile_lib(
        "extension_receiver_inner_constructor",
        "package dep\n\
         class Outer(val prefix: String) {\n\
         \x20 inner class Inner(private val suffix: String) {\n\
         \x20   fun text(): String = prefix + suffix\n\
         \x20 }\n\
         }\n",
    ) else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let source = "package app\n\
                  import dep.Outer\n\
                  fun Outer.make(): String = Inner(\"K\").text()\n\
                  fun box(): String = Outer(\"O\").make()\n";
    let Some(classes) = common::compile_in_process(
        source,
        "Use",
        &[lib.clone(), stdlib.clone()],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    match common::run_box(&classes, "app.UseKt", &[lib, stdlib]) {
        Some(output) => assert_eq!(output.trim(), "OK", "box() = {output:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

/// A companion declared in ANOTHER FILE of the same module is a class constant in the referencing
/// class (`Owner.make()` loads `Owner$Companion`), so kotlinc gives it an `InnerClasses` entry
/// (`public static final Companion=class app/Owner$Companion of class app/Owner`). The classpath
/// resolver cannot see source classes, so this exercises the module-symbols layer.
#[test]
fn cross_file_source_companion_gets_inner_classes_entry() {
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process_files(
        &[
            (
                "Owner",
                "package app\nclass Owner { companion object { fun make(): Int = 1 } }\n",
            ),
            ("Use", "package app\nfun use(): Int = Owner.make()\n"),
        ],
        &[stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let names: Vec<&String> = classes.iter().map(|(name, _)| name).collect();
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/UseKt")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .unwrap_or_else(|| panic!("emitted facade; classes: {names:?}"));
    assert_eq!(
        emitted
            .inner_classes
            .iter()
            .find(|entry| entry.inner == "app/Owner$Companion"),
        Some(&krusty::jvm::classreader::InnerClassRef {
            inner: "app/Owner$Companion".to_string(),
            outer: Some("app/Owner".to_string()),
            name: Some("Companion".to_string()),
            // kotlinc 2.4.10: `public static final` (0x19).
            access: 0x0019,
        }),
        "entries: {:?}",
        emitted.inner_classes,
    );
}

/// A nested annotation class applied to a declaration appears ONLY as a descriptor inside the
/// `RuntimeInvisibleAnnotations` attribute — never as a `new`/`checkcast` class constant — yet
/// kotlinc still records an `InnerClasses` entry for it (access 0x2609: public static interface
/// abstract annotation, verified against kotlinc 2.4.10). Same-file shape.
#[test]
fn annotation_only_reference_gets_inner_classes_entry() {
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process_files(
        &[(
            "Ann",
            "package app\nclass Outer { annotation class Mark }\n@Outer.Mark class Tagged\n",
        )],
        &[stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/Tagged")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("emitted class");
    assert_eq!(
        emitted
            .inner_classes
            .iter()
            .find(|entry| entry.inner == "app/Outer$Mark"),
        Some(&krusty::jvm::classreader::InnerClassRef {
            inner: "app/Outer$Mark".to_string(),
            outer: Some("app/Outer".to_string()),
            name: Some("Mark".to_string()),
            access: 0x2609,
        }),
        "entries: {:?}",
        emitted.inner_classes,
    );
}

/// A nested ENUM used purely as an annotation ARGUMENT is also a reference: kotlinc 2.4.10 records
/// `public static final enum Color=class app/Holder$Color of class app/Holder` on the annotated
/// class even though the enum type only appears as a descriptor inside the element_value.
#[test]
fn enum_annotation_argument_gets_inner_classes_entry() {
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process_files(
        &[(
            "EnumArg",
            "package app\nclass Holder { enum class Color { RED } }\nannotation class Paint(val c: Holder.Color)\n@Paint(Holder.Color.RED) class Wall\n",
        )],
        &[stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/Wall")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("emitted class");
    let entry = emitted
        .inner_classes
        .iter()
        .find(|entry| entry.inner == "app/Holder$Color")
        .unwrap_or_else(|| panic!("no Holder$Color entry: {:?}", emitted.inner_classes));
    assert_eq!(entry.outer.as_deref(), Some("app/Holder"));
    assert_eq!(entry.name.as_deref(), Some("Color"));
    // public static final enum
    assert_eq!(entry.access, 0x4019, "access 0x{:04x}", entry.access);
}

/// A nested class LITERAL as an annotation argument (`@Uses(Holder.Nested::class)`) is a reference
/// too: kotlinc 2.4.10 records `public static final Nested=class app/Holder$Nested of class
/// app/Holder` on the annotated class.
#[test]
fn class_literal_annotation_argument_gets_inner_classes_entry() {
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process_files(
        &[(
            "KClassArg",
            "package app\nimport kotlin.reflect.KClass\nclass Holder { class Nested }\nannotation class Uses(val k: KClass<*>)\n@Uses(Holder.Nested::class) class Site\n",
        )],
        &[stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/Site")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("emitted class");
    let entry = emitted
        .inner_classes
        .iter()
        .find(|entry| entry.inner == "app/Holder$Nested")
        .unwrap_or_else(|| panic!("no Holder$Nested entry: {:?}", emitted.inner_classes));
    assert_eq!(entry.outer.as_deref(), Some("app/Holder"));
    assert_eq!(entry.name.as_deref(), Some("Nested"));
    assert_eq!(entry.access, 0x0019, "access 0x{:04x}", entry.access);
}

/// The CLASSPATH variant of the annotation-only reference: the nested annotation lives in a
/// dependency jar, so the entry's details come from the classpath resolver — but only if the
/// annotation DESCRIPTOR counts as a reference.
#[test]
fn classpath_annotation_only_reference_gets_inner_classes_entry() {
    let Some(lib) = common::compile_lib(
        "inner_classes_ann",
        "package dep\nclass Outer { annotation class Mark }\n",
    ) else {
        return;
    };
    let expected = std::fs::read(lib.join("dep/Outer$Mark.class"))
        .ok()
        .and_then(|bytes| parse_class(&bytes).ok())
        .and_then(|class| {
            class
                .inner_classes
                .into_iter()
                .find(|entry| entry.inner == "dep/Outer$Mark")
        })
        .expect("dependency self entry");
    let stdlib = common::stdlib_jar();
    let Some(classes) = common::compile_in_process(
        "package app\n@dep.Outer.Mark class Tagged\n",
        "Use",
        &[lib, stdlib],
        Some(common::jdk_modules().as_path()),
    ) else {
        panic!("compile");
    };
    let emitted = classes
        .iter()
        .find(|(name, _)| name == "app/Tagged")
        .and_then(|(_, bytes)| parse_class(bytes).ok())
        .expect("emitted class");
    assert!(
        emitted.inner_classes.contains(&expected),
        "entries: {:?} expected: {:?}",
        emitted.inner_classes,
        expected,
    );
}

#[test]
fn generic_nested_dependency_constructor_keeps_its_erased_physical_descriptor() {
    let library = r#"
        package dep

        abstract class Root<Nested : Root.Base<*>> {
            open class Base<V>(val value: V)
        }
    "#;
    let main = r#"
        package app

        import dep.Root

        class Payload(val text: String)

        class Derived : Root<Derived.Nested>() {
            class Nested : Base<Payload>(Payload("O")) {
                fun result(): String = Base(value).value.text + "K"
            }
        }

        fun box(): String = Derived.Nested().result()
    "#;

    let Some(output) =
        common::expect_box_run_against("generic_nested_ctor_descriptor", library, main)
    else {
        return;
    };
    assert_eq!(output, "OK");
}
