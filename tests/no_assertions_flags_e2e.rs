//! `-Xno-param-assertions` / `-Xno-call-assertions`: compare the shipping CLI's output with the
//! reference compiler under the same flags. intellij-community sets both per module
//! (`build/compiler-options.bzl`), so an emitter-only test is insufficient: the command-line option,
//! backend configuration, lowering records, derived setter guards, constant pool, and debug tables
//! all have to agree.

use std::path::Path;
use std::sync::OnceLock;

use super::common;

/// Every place that currently emits `checkNotNullParameter`, plus a constructor wide enough for an
/// incorrect post-guard debug offset to point beyond the end of its method.
const PARAMETER_SOURCE: &str = r#"
class Box {
    var prop: String = "a"
    lateinit var later: String
}
class Wide(val a: String, val b: String, val c: String, val d: String, val e: String, val f: String)
fun String.ext(other: String): Int = this.length + other.length
fun withDefault(a: String, b: String = "d"): Int = a.length + b.length
interface I { fun m(s: String): Int = s.length }
class Impl : I
fun box(): String {
    val b = Box(); b.prop = "z"
    val wide = Wide("1", "2", "3", "4", "5", "6")
    return if ("x".ext("y") == 2 && withDefault("q") == 2 && Impl().m("ab") == 2 &&
        b.prop == "z" && wide.a == "1") "OK" else "fail"
}
"#;

/// A platform-typed value from a Java class is the call-assertion shape affected by
/// `-Xno-call-assertions`. The function need not run: the test inspects its emitted call sites.
const CALL_SOURCE: &str = r#"
fun fromJava(): String = java.lang.System.getProperty("krusty.assertion.fixture")
fun box(): String = "OK"
"#;

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

fn compile_krusty(src: &str, stem: &str, flag: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate krusty assertion fixture");
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create krusty output");
    std::fs::write(&source, src).expect("write krusty assertion fixture");
    let mut command = std::process::Command::new(common::krusty_binary());
    command.args(["-d", output.to_str().expect("UTF-8 output")]);
    if let Some(flag) = flag {
        command.arg(flag);
    }
    let result = command.arg(&source).output().expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty failed under {}: stdout={} stderr={}",
        flag.unwrap_or("default options"),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!classes.is_empty(), "krusty emitted no classes");
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn compile_reference(src: &str, stem: &str, flag: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate kotlinc assertion fixture");
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, src).expect("write kotlinc assertion fixture");
    let mut args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
    ];
    if let Some(flag) = flag {
        args.push(flag.to_string());
    }
    args.push(source.to_string_lossy().into_owned());
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(
        code,
        0,
        "kotlinc failed under {}: {stderr}",
        flag.unwrap_or("default options")
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!classes.is_empty(), "kotlinc emitted no classes");
    let _ = std::fs::remove_dir_all(work);
    classes
}

/// Count `Intrinsics.<name>` CALL SITES across emitted classes. Raw class bytes would also count
/// unused constant-pool strings; disassembly measures the instructions that actually execute.
fn intrinsic_calls(classes: &[(String, Vec<u8>)], name: &str, tag: &str) -> usize {
    let work = common::scratch_dir().expect("allocate javap assertion fixture");
    let mut arguments = vec!["-p".to_string(), "-c".to_string()];
    for (internal, bytes) in classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create class parent");
        }
        std::fs::write(&path, bytes).expect("write class for javap");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let borrowed = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    let text =
        common::javap(&borrowed).unwrap_or_else(|| panic!("pooled javap unavailable for {tag}"));
    let needle = format!("Intrinsics.{name}");
    let count = text.lines().filter(|line| line.contains(&needle)).count();
    let _ = std::fs::remove_dir_all(work);
    count
}

struct ParameterBuilds {
    krusty_default: Vec<(String, Vec<u8>)>,
    krusty_without: Vec<(String, Vec<u8>)>,
    reference_default: Vec<(String, Vec<u8>)>,
    reference_without: Vec<(String, Vec<u8>)>,
}

fn parameter_builds() -> &'static ParameterBuilds {
    static BUILDS: OnceLock<ParameterBuilds> = OnceLock::new();
    BUILDS.get_or_init(|| ParameterBuilds {
        krusty_default: compile_krusty(PARAMETER_SOURCE, "Assertions", None),
        krusty_without: compile_krusty(
            PARAMETER_SOURCE,
            "Assertions",
            Some("-Xno-param-assertions"),
        ),
        reference_default: compile_reference(PARAMETER_SOURCE, "Assertions", None),
        reference_without: compile_reference(
            PARAMETER_SOURCE,
            "Assertions",
            Some("-Xno-param-assertions"),
        ),
    })
}

fn class_names(classes: &[(String, Vec<u8>)]) -> Vec<&str> {
    classes.iter().map(|(name, _)| name.as_str()).collect()
}

#[test]
fn no_param_assertions_matches_kotlincs_guard_contract() {
    let builds = parameter_builds();
    let krusty_default = intrinsic_calls(
        &builds.krusty_default,
        "checkNotNullParameter",
        "krusty default",
    );
    let reference_default = intrinsic_calls(
        &builds.reference_default,
        "checkNotNullParameter",
        "kotlinc default",
    );
    assert!(krusty_default > 0, "krusty's default must emit guards");
    assert!(reference_default > 0, "kotlinc's default must emit guards");
    assert_eq!(
        intrinsic_calls(
            &builds.krusty_without,
            "checkNotNullParameter",
            "krusty -Xno-param-assertions",
        ),
        0,
        "no parameter guard may survive in krusty output"
    );
    assert_eq!(
        intrinsic_calls(
            &builds.reference_without,
            "checkNotNullParameter",
            "kotlinc -Xno-param-assertions",
        ),
        0,
        "the reference fixture must exercise the same flag contract"
    );
}

#[test]
fn no_param_assertions_preserves_the_reference_class_set() {
    let builds = parameter_builds();
    assert_eq!(
        class_names(&builds.krusty_default),
        class_names(&builds.krusty_without),
        "krusty may remove instructions, not classes"
    );
    assert_eq!(
        class_names(&builds.reference_default),
        class_names(&builds.reference_without),
        "the kotlinc control must have a stable class set"
    );
}

#[test]
fn stripped_guards_leave_valid_debug_tables_and_behavior() {
    let classes = &parameter_builds().krusty_without;
    let box_class = common::find_box_class(classes).expect("no box class emitted");
    let result = common::run_box(classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for assertion-flag behavior test");
    assert_eq!(result, "OK");
}

#[test]
fn no_call_assertions_matches_kotlincs_platform_call_contract() {
    let krusty = compile_krusty(CALL_SOURCE, "Calls", Some("-Xno-call-assertions"));
    let reference_default = compile_reference(CALL_SOURCE, "Calls", None);
    let reference_without = compile_reference(CALL_SOURCE, "Calls", Some("-Xno-call-assertions"));
    assert!(
        intrinsic_calls(
            &reference_default,
            "checkNotNullExpressionValue",
            "kotlinc default call assertions",
        ) > 0,
        "the fixture must make kotlinc emit a platform-value assertion"
    );
    assert_eq!(
        intrinsic_calls(
            &reference_without,
            "checkNotNullExpressionValue",
            "kotlinc -Xno-call-assertions",
        ),
        0,
        "the reference compiler must remove the platform-value assertion"
    );
    assert_eq!(
        intrinsic_calls(
            &krusty,
            "checkNotNullExpressionValue",
            "krusty -Xno-call-assertions",
        ),
        0,
        "krusty output under the flag must match kotlinc's assertion contract"
    );
}
