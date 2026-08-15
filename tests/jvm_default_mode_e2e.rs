//! `-jvm-default`: the JVM shape an interface's members with bodies are compiled into.
//!
//! Every assertion here is measured against the reference kotlinc, not asserted from the spec: the
//! three modes differ in which classes exist, which methods are abstract, and what `@Metadata`
//! records, and only a differential catches a shape that is plausible but not what kotlinc emits.

use super::common;

use krusty::jvm::ir_emit::JvmDefaultMode;
use std::path::Path;

/// An interface exercising every member kind whose realization `-jvm-default` changes: a property
/// with a default getter, a method with a body, a method with a default parameter value (which adds
/// a `$default` stub), and an abstract method that must stay abstract in all three modes.
const INTERFACE_SOURCE: &str = r#"
interface I {
    val x: Int get() = 1
    fun f(): String = "f" + x
    fun g(a: Int = 5): String = "g$a"
    fun abs(): Int
}

class C : I {
    override fun abs(): Int = 7
}

fun box(): String {
    val c: I = C()
    return if (c.f() == "f1" && c.g() == "g5" && c.x == 1 && c.abs() == 7) "OK" else "fail"
}
"#;

fn class_names(classes: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut names: Vec<String> = classes.iter().map(|(name, _)| name.clone()).collect();
    names.sort();
    names
}

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            let name = path
                .strip_prefix(root)
                .expect("class below output root")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            classes.push((name, std::fs::read(path).expect("read emitted class")));
        }
    }
}

fn compile(mode: JvmDefaultMode) -> Vec<(String, Vec<u8>)> {
    let flag = match mode {
        JvmDefaultMode::Enable => "enable",
        JvmDefaultMode::NoCompatibility => "no-compatibility",
        JvmDefaultMode::Disable => "disable",
    };
    let work = common::scratch_dir().expect("allocate krusty fixture");
    let source = work.join("I.kt");
    let output = work.join("out");
    std::fs::write(&source, INTERFACE_SOURCE).expect("write krusty fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .arg(format!("-jvm-default={flag}"))
        .arg(&source)
        .output()
        .expect("run krusty");
    assert!(
        result.status.success(),
        "krusty failed under {mode:?}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn compile_reference(flag: &str) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate kotlinc fixture");
    let source = work.join("I.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, INTERFACE_SOURCE).expect("write kotlinc fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        format!("-jvm-default={flag}"),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc failed under {flag}: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn public_method_shape(
    classes: &[(String, Vec<u8>)],
    class_name: &str,
) -> Vec<(String, String, u16)> {
    let bytes = classes
        .iter()
        .find_map(|(name, bytes)| (name == class_name).then_some(bytes))
        .unwrap_or_else(|| panic!("missing {class_name}.class"));
    let class = krusty::jvm::classreader::parse_class(bytes)
        .unwrap_or_else(|_| panic!("parse {class_name}.class"));
    let mut methods = class
        .methods
        .iter()
        .filter(|method| method.is_public())
        .map(|method| {
            (
                method.name.clone(),
                method.descriptor.clone(),
                method.access,
            )
        })
        .collect::<Vec<_>>();
    methods.sort();
    methods
}

/// kotlinc's own default emits both the interface default methods AND the `$DefaultImpls`
/// compatibility copy, so krusty's default must keep producing that class.
#[test]
fn enable_keeps_the_default_impls_compatibility_class() {
    let names = class_names(&compile(JvmDefaultMode::Enable));
    assert!(
        names.iter().any(|name| name == "I$DefaultImpls"),
        "`enable` keeps the compatibility holder: {names:?}"
    );
}

/// The mode intellij-community builds with (`-Xjvm-default=all`). kotlinc emits NO `$DefaultImpls`
/// at all — a build that links against these classes would resolve a holder that should not exist.
#[test]
fn no_compatibility_emits_no_default_impls_class() {
    let names = class_names(&compile(JvmDefaultMode::NoCompatibility));
    assert!(
        !names.iter().any(|name| name.contains("DefaultImpls")),
        "`no-compatibility` emits no compatibility holder: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "I"),
        "the interface itself is still emitted: {names:?}"
    );
}

/// `disable` needs a different emitter. The CLI must stop before compilation; printing a warning
/// and continuing under `enable` would leave a successful build containing the wrong class shape.
#[test]
fn disable_is_rejected_without_emitting_fallback_classes() {
    let work = common::scratch_dir().expect("allocate disable fixture");
    let source = work.join("I.kt");
    let output = work.join("out");
    std::fs::write(&source, "interface I { fun f(): Int = 1 }").expect("write disable fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .arg("-jvm-default=disable")
        .arg(&source)
        .output()
        .expect("run krusty");
    assert!(!result.status.success(), "disable unexpectedly compiled");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("does not emit that interface shape"),
        "unexpected diagnostic: {stderr}"
    );
    assert!(
        !output.exists(),
        "a rejected output-shape option must not leave compiler output"
    );
    let _ = std::fs::remove_dir_all(work);
}

/// The set of class files krusty produces must match kotlinc's for the same sources and the same
/// `-jvm-default` value. This is the check that would have caught krusty emitting its one interface
/// strategy regardless of the flag.
#[test]
fn the_emitted_class_set_matches_kotlinc_per_mode() {
    for (mode, flag) in [
        (JvmDefaultMode::Enable, "enable"),
        (JvmDefaultMode::NoCompatibility, "no-compatibility"),
    ] {
        let reference = class_names(&compile_reference(flag));
        let ours = class_names(&compile(mode));
        assert_eq!(
            ours, reference,
            "class set diverges under -jvm-default={flag}"
        );
    }
}

/// `no-compatibility` is claimed as a fully modelled class shape, so compare the public methods and
/// their concrete/abstract/static realization as well as the class set. A class-name-only check
/// cannot distinguish a correctly emitted interface from one whose methods live in the wrong owner.
#[test]
fn no_compatibility_public_method_shape_matches_kotlinc() {
    let ours = compile(JvmDefaultMode::NoCompatibility);
    let reference = compile_reference("no-compatibility");
    for class_name in ["I", "C"] {
        assert_eq!(
            public_method_shape(&ours, class_name),
            public_method_shape(&reference, class_name),
            "{class_name} public method shape"
        );
    }
}

/// `jvmClassFlags` (`Class` JvmProtoBuf extension field 104) records the shape a consumer will find:
/// bit 0 "bodies live on the interface", bit 1 "a compatibility copy exists". kotlinc writes 3 for
/// `enable` and 1 for `no-compatibility`.
///
/// Asserted on the EMITTED metadata, not on the enum: a mode that changed the class set but left the
/// metadata saying "a compatibility copy exists" would publish a class file whose bytes and whose
/// metadata disagree, which is exactly what a consumer compiling against it would act on.
#[test]
fn the_emitted_metadata_records_the_mode_that_produced_the_class() {
    for (mode, flag, trailer) in [
        (JvmDefaultMode::Enable, "enable", "\\u0006\\u0003"),
        (
            JvmDefaultMode::NoCompatibility,
            "no-compatibility",
            "\\u0006\\u0001",
        ),
    ] {
        let classes = compile(mode);
        let metadata = interface_metadata_line(&classes);
        let reference_metadata = interface_metadata_line(&compile_reference(flag));
        assert!(
            reference_metadata.contains(trailer),
            "kotlinc {flag}: expected jvmClassFlags trailer {trailer} in {reference_metadata}"
        );
        assert!(
            metadata.contains(trailer),
            "{mode:?}: expected the jvmClassFlags trailer {trailer} in {metadata}"
        );
    }
}

/// The `d1` line of `I`'s `@Metadata`, via the required pooled `javap` tool.
fn interface_metadata_line(classes: &[(String, Vec<u8>)]) -> String {
    let dir = common::scratch_dir().expect("allocate metadata fixture");
    for (name, bytes) in classes {
        let path = dir.join(format!("{name}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create metadata class directory");
        }
        std::fs::write(path, bytes).expect("write metadata class");
    }
    let class_file = dir.join("I.class");
    let text = common::javap(&["-v", "-p", &class_file.to_string_lossy()])
        .expect("JVM unavailable for metadata inspection");
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("d1="))
        .expect("interface Metadata.d1")
        .to_string();
    let _ = std::fs::remove_dir_all(&dir);
    line
}

/// The shape change must not change behavior: the same program produces the same result whichever
/// compatibility strategy its interfaces were compiled with.
#[test]
fn a_program_behaves_the_same_under_every_modelled_mode() {
    for mode in [JvmDefaultMode::Enable, JvmDefaultMode::NoCompatibility] {
        let classes = compile(mode);
        let Some(box_class) = common::find_box_class(&classes) else {
            panic!("{mode:?}: no box class emitted");
        };
        let result = common::run_box(&classes, &box_class, &[common::stdlib_jar()])
            .expect("JVM unavailable for jvm-default behavior test");
        assert_eq!(result, "OK", "{mode:?}");
    }
}
