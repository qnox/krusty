//! Every DECLARED classifier publishes the same JVM class `Signature` kotlinc does — the recorded
//! signature of a generic declaration, or the parameterized `java/lang/Enum<E>` supertype an enum
//! always has. The declaration's KIND does not decide this: a generic interface carried none for as
//! long as interfaces were emitted through their own writer, so `interface Iface<B>` reached the
//! classpath as a raw type while `class Klass<B>` did not.

use std::fs;
use std::path::{Path, PathBuf};

use super::common;

const SOURCE: &str = r#"
    interface Iface<B> {
        fun payload(): B
    }

    interface SubIface<B> : Iface<B>

    interface Plain {
        fun tag(): String
    }

    open class Klass<B>(val value: B)

    class Sub<B>(value: B) : Klass<B>(value), Iface<B> {
        override fun payload(): B = value
    }

    data class Duo<A, B>(val a: A, val b: B)

    class Bounded<T : Comparable<T>>(val t: T)

    class Outer<A> {
        inner class Inner<B>(val b: B)
    }

    object Single : Plain {
        override fun tag(): String = "single"
    }

    class Implementor : Iface<String> {
        override fun payload(): String = "implementor"
    }

    enum class Color {
        RED,
        GREEN,
    }

    enum class Tagged : Iface<String> {
        ONE,
        TWO,
        ;

        override fun payload(): String = name
    }

    enum class Empty
"#;

/// The class-level `Signature` attribute, or `None` when the class carries none. javap prints the
/// class attribute unindented and every member attribute indented, so the column separates them.
fn class_signature(class_file: &Path) -> Option<String> {
    let disassembly = common::javap(&["-v", &class_file.to_string_lossy()])
        .unwrap_or_else(|| panic!("javap failed for {}", class_file.display()));
    disassembly
        .lines()
        .find(|line| line.starts_with("Signature:"))
        .and_then(|line| line.split("// ").nth(1))
        .map(str::trim)
        .map(str::to_string)
}

fn write_classes(classes: &[(String, Vec<u8>)], dir: &Path) {
    for (internal, bytes) in classes {
        let path = dir.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create class directory");
        }
        fs::write(&path, bytes).expect("write class file");
    }
}

#[test]
fn every_classifier_form_publishes_kotlincs_class_signature() {
    let work = common::scratch_dir().expect("allocate a scratch directory");
    let reference = work.join("reference");
    let ours = work.join("krusty");
    fs::create_dir_all(&reference).expect("create reference directory");
    fs::create_dir_all(&ours).expect("create krusty directory");
    let source_path = work.join("Classifiers.kt");
    fs::write(&source_path, SOURCE).expect("write fixture source");

    let (code, diagnostics) = common::kotlinc_compile(&[
        "-nowarn".to_string(),
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])
    .expect("run the reference compiler");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {diagnostics}");

    let classes = common::compile_in_process(SOURCE, "Classifiers", &[common::stdlib_jar()], None)
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                common::front_end_diagnostics(SOURCE, &[common::stdlib_jar()], None)
            )
        });
    write_classes(&classes, &ours);

    let named = [
        "Iface",
        "SubIface",
        "Plain",
        "Klass",
        "Sub",
        "Duo",
        "Bounded",
        "Outer",
        "Outer$Inner",
        "Single",
        "Implementor",
        "Color",
        "Tagged",
        "Empty",
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for name in named {
        let file = format!("{name}.class");
        let expected = class_signature(&reference.join(&file));
        let actual = class_signature(&ours.join(&file));
        if expected != actual {
            mismatches.push(format!("{name}: kotlinc {expected:?}, krusty {actual:?}"));
        }
    }
    let _: PathBuf = work.clone();
    let _ = fs::remove_dir_all(&work);
    assert!(
        mismatches.is_empty(),
        "class-level Signature must match kotlinc for every classifier form: {mismatches:?}"
    );
}
