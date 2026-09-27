//! kotlinc sorts a class's `InnerClasses` rows by each class's `fqNameWhenAvailable`, and an
//! anonymous object's name runs through the declaration kotlinc's IR moved its initializer into.
//!
//! A property initializer or `init` block of a class runs in the constructor, so an object there is
//! `Holder.<init>.<no name provided>`; an object's, a companion's or a top-level initializer runs in
//! the static initializer, `<clinit>`, and a companion's is its outer class's. Both sort ahead of a
//! named nested class, since `<` precedes every letter, which a table sorted by the classes' own
//! names alone gets backwards.
use super::common;
use krusty::jvm::classreader::{parse_class, InnerClassRef};

/// Compile `source` with kotlinc and krusty and require each of `classes` to carry kotlinc's
/// `InnerClasses` rows, in kotlinc's order.
fn assert_same_inner_classes(stem: &str, source: &str, classes: &[&str]) {
    let dir = common::scratch_dir().expect("scratch directory");
    let reference = dir.join("ref");
    std::fs::create_dir_all(&reference).expect("reference output directory");
    let source_path = dir.join(format!("{stem}.kt"));
    std::fs::write(&source_path, source).expect("fixture source");
    let args = [
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference kotlinc is provisioned");
    assert_eq!(code, 0, "{stem}: kotlinc failed: {stderr}");

    let classpath = [common::stdlib_jar()];
    let jdk = common::jdk_modules();
    let compiled = common::compile_in_process(source, stem, &classpath, Some(jdk.as_path()))
        .unwrap_or_else(|| {
            panic!(
                "{stem}: krusty rejected the fixture: {:?}",
                common::front_end_diagnostics(source, &classpath, Some(jdk.as_path()))
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
    assert_same_inner_classes("ConstructorObjects", &source, &["Holder"]);
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
    assert_same_inner_classes("StaticObjects", &source, &["Registry"]);
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
    assert_same_inner_classes("CompanionObjects", &source, &["Outer", "Outer$Companion"]);
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
    assert_same_inner_classes(
        "InterfaceCompanionObjects",
        &source,
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
    assert_same_inner_classes("ContinuationClasses", &source, &["Task"]);
}
