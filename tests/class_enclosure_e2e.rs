//! The `EnclosingMethod` and `InnerClasses` attributes of a class declared inside another
//! declaration's body, as kotlinc writes them.
//!
//! kotlinc names the scope the class was lowered in, not the shape of its binary name: a
//! function's classes point at that function (a lambda's at the named function around it), a
//! top-level property initializer's at the file facade with no method, an instance property's or
//! `init` block's at the primary constructor, an object's property at the object itself, and a
//! companion's property at the class that stores it. A callable-reference class lists only itself
//! in `InnerClasses`, as a `static final synthetic` class with no outer class.

use super::common;

const SOURCE: &str = "fun label(text: String): String = text + text\n\
object Registry {\n\
\x20   val tag: (String) -> String = ::label\n\
\x20   val token: Any = object {}\n\
}\n\
class Holder(val size: Int) {\n\
\x20   val tag: (String) -> String = ::label\n\
\x20   companion object { val shared: (String) -> String = ::label }\n\
}\n\
class Scope(val base: Int) {\n\
\x20   val getterLocal: Any\n\
\x20       get() {\n\
\x20           class GetterLocal\n\
\x20           return GetterLocal()\n\
\x20       }\n\
\x20   var setterLocal: Int = 0\n\
\x20       set(value) {\n\
\x20           class SetterLocal\n\
\x20           SetterLocal()\n\
\x20           field = value\n\
\x20       }\n\
\x20   constructor(text: String) : this(text.length) {\n\
\x20       class ConstructorLocal\n\
\x20       ConstructorLocal()\n\
\x20   }\n\
}\n\
fun outer(): Int {\n\
\x20   class Local { fun tag(): (String) -> String = ::label }\n\
\x20   val probe = object { val tag: (String) -> String = ::label }\n\
\x20   return (Local().tag()(\"b\") + probe.tag(\"c\")).length\n\
}\n\
fun factory(): () -> Any = { object {} }\n\
val token: Any = object {}\n\
fun <T> generic(value: T): (String) -> String = ::label\n\
fun box(): String {\n\
\x20   val scope = Scope(\"a\")\n\
\x20   scope.setterLocal = 3\n\
\x20   val total = outer() + Registry.tag(\"a\").length + Holder(1).tag(\"a\").length +\n\
\x20       Holder.shared(\"a\").length + generic(1)(\"a\").length\n\
\x20   return if (total == 12 && scope.setterLocal == 3 && scope.getterLocal != token &&\n\
\x20       factory()() != token && Registry.token != token) \"OK\" else \"fail: $total\"\n\
}\n";

/// Reference classes whose every byte matches kotlinc's, the enclosure included.
const CARRIERS: &[&str] = &[
    "EnclosureKt$generic$1",
    "EnclosureKt$outer$Local$tag$1",
    "EnclosureKt$outer$probe$1$tag$1",
    "Holder$tag$1",
    "Holder$Companion$shared$1",
    "Registry$tag$1",
];

/// Classes whose enclosure matches kotlinc's while other parts (their `@Metadata`, how they store
/// captured values) still differ.
const ENCLOSED: &[&str] = &[
    "EnclosureKt$token$1",
    "EnclosureKt$factory$1$1",
    "EnclosureKt$outer$Local",
    "EnclosureKt$outer$probe$1",
    "Registry$token$1",
    "Holder$Companion",
];

/// Local classes in callable shapes that previously reached JVM emission without a recorded scope.
/// Their `InnerClasses` simple-name parity belongs to the local-name migration in #1219; this PR
/// compares the complete owner, callable name and descriptor of their `EnclosingMethod` attribute.
const CALLABLE_ENCLOSED: &[&str] = &[
    "Scope$getterLocal$GetterLocal",
    "Scope$setterLocal$SetterLocal",
    "Scope$ConstructorLocal",
];

struct Compiled {
    dir: std::path::PathBuf,
    reference: std::path::PathBuf,
    ours: std::path::PathBuf,
}

impl Drop for Compiled {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn compile_both() -> Option<Compiled> {
    let dir = common::scratch_dir()?;
    let reference = dir.join("ref");
    let ours = dir.join("out");
    std::fs::create_dir_all(&reference).ok()?;
    std::fs::create_dir_all(&ours).ok()?;
    let source_path = dir.join("Enclosure.kt");
    std::fs::write(&source_path, SOURCE).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted =
        common::compile_in_process_metadata_cp(SOURCE, "Enclosure", &[common::stdlib_jar()])
            .expect("krusty compiles the enclosed classes");
    for (name, bytes) in &emitted {
        std::fs::write(ours.join(format!("{name}.class")), bytes).ok()?;
    }
    Some(Compiled {
        dir,
        reference,
        ours,
    })
}

#[test]
fn reference_carriers_are_byte_identical_to_kotlinc() {
    let Some(compiled) = compile_both() else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for class in CARRIERS {
        let file = format!("{class}.class");
        let reference = std::fs::read(compiled.reference.join(&file)).expect("kotlinc class");
        let ours = std::fs::read(compiled.ours.join(&file)).expect("krusty class");
        assert!(
            ours == reference,
            "{class}: class file differs from kotlinc"
        );
    }
}

/// The `InnerClasses`, `EnclosingMethod` and `SourceFile` attributes, in order, of `class` under
/// `root`, with constant-pool indices removed.
fn enclosure(root: &std::path::Path, class: &str) -> String {
    let dump = common::javap(&["-v", "-cp", &root.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("javap {class}"));
    dump.lines()
        .skip_while(|line| !line.starts_with("InnerClasses:"))
        .take_while(|line| !line.starts_with("RuntimeVisibleAnnotations:"))
        .map(|line| {
            line.split_whitespace()
                .filter(|word| !word.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn enclosing_callable(root: &std::path::Path, class: &str) -> String {
    let dump = common::javap(&["-v", "-cp", &root.to_string_lossy(), class])
        .unwrap_or_else(|| panic!("javap {class}"));
    let enclosing = dump
        .lines()
        .find(|line| line.trim_start().starts_with("EnclosingMethod:"))
        .unwrap_or_else(|| panic!("{class}: EnclosingMethod attribute"));
    let owner_and_method = enclosing
        .split_once("// ")
        .map(|(_, value)| value.trim())
        .unwrap_or_else(|| panic!("{class}: EnclosingMethod owner and name"));
    let name_and_type = enclosing
        .split_once('.')
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .unwrap_or_else(|| panic!("{class}: EnclosingMethod name-and-type index"));
    let prefix = format!("{name_and_type} = NameAndType");
    let signature = dump
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))
        .and_then(|line| line.split_once("// "))
        .map(|(_, value)| value.trim())
        .unwrap_or_else(|| panic!("{class}: EnclosingMethod descriptor"));
    format!("{owner_and_method} {signature}")
}

#[test]
fn enclosed_classes_name_the_scope_kotlinc_lowered_them_in() {
    let Some(compiled) = compile_both() else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for class in ENCLOSED {
        let reference = enclosure(&compiled.reference, class);
        assert!(
            reference.contains("SourceFile"),
            "{class}: kotlinc wrote its enclosure"
        );
        assert_eq!(
            enclosure(&compiled.ours, class),
            reference,
            "{class}: enclosure differs from kotlinc"
        );
    }
}

#[test]
fn accessors_and_secondary_constructors_record_the_exact_callable_scope() {
    let Some(compiled) = compile_both() else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for class in CALLABLE_ENCLOSED {
        assert_eq!(
            enclosing_callable(&compiled.ours, class),
            enclosing_callable(&compiled.reference, class),
            "{class}: callable enclosure differs from kotlinc"
        );
    }
}

#[test]
fn enclosed_classes_run() {
    common::expect_box_same_as_kotlinc(SOURCE, "EnclosureRun");
}
