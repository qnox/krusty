//! The intrinsic null check kotlinc inserts when a PLATFORM value (`T!`) is narrowed to an
//! explicitly non-null Kotlin type.
//!
//! A Java call result is `T!`: usable as `T` or `T?`. Where the source commits it to a declared
//! non-null type, kotlinc emits `dup; ldc "<expression>"; invokestatic
//! Intrinsics.checkNotNullExpressionValue`, so a null fails at the boundary where it enters the
//! non-null world instead of far away. Descriptors and `@NotNull` annotations cannot express that:
//! they already agreed while the check was missing, so these tests assert the emitted call sites AND
//! the runtime failure, both against kotlinc 2.4.10.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::common;

/// Every position measured against kotlinc 2.4.10. The guarded ones commit a platform CALL result to
/// a declared non-null type; the controls keep the value flexible (`T!` stays `T!`) or narrow
/// something that is not a call result, and kotlinc leaves those alone.
const POSITIONS: &str = r#"
val topLevelProperty: String = System.getenv("KRUSTY_ABSENT_1")

class Member {
    val memberProperty: String = System.getenv("KRUSTY_ABSENT_2")
}

fun explicitLocal(): Int {
    val local: String = System.getenv("KRUSTY_ABSENT_3")
    return local.length
}

fun takesNonNull(value: String): Int = value.length

fun argument(): Int = takesNonNull(System.getenv("KRUSTY_ABSENT_4"))

fun returned(): String = System.getenv("KRUSTY_ABSENT_5")

fun lambdaParameter(): Int {
    val f: (String) -> Int = { it.length }
    return f(System.getenv("KRUSTY_ABSENT_6"))
}

fun assignment(): Int {
    var v: String = "x"
    v = System.getenv("KRUSTY_ABSENT_7")
    return v.length
}

fun inferredLocal(): Int {
    val local = System.getenv("KRUSTY_ABSENT_8")
    return local.length
}

fun javaMemberReceiver(): Int = System.getenv("KRUSTY_ABSENT_9").length

fun nullableLocal(): Int {
    val local: String? = System.getenv("KRUSTY_ABSENT_10")
    return local?.length ?: 0
}

fun elvis(): String = System.getenv("KRUSTY_ABSENT_11") ?: "d"

fun stringTemplate(): String = "v=${System.getenv("KRUSTY_ABSENT_12")}"

fun conditionalLocal(c: Boolean): Int {
    val v: String = if (c) System.getenv("KRUSTY_ABSENT_13") else "x"
    return v.length
}

fun conditionalReturn(c: Boolean): String = if (c) System.getenv("KRUSTY_ABSENT_14") else "x"

fun whenBranch(k: Int): String = when (k) {
    1 -> System.getenv("KRUSTY_ABSENT_15")
    else -> "x"
}

fun elvisRight(x: String?): Int {
    val v: String = x ?: System.getenv("KRUSTY_ABSENT_16")
    return v.length
}

fun nestedConditional(c: Boolean, d: Boolean): Int {
    val v: String = if (c) { if (d) System.getenv("KRUSTY_ABSENT_17") else "a" } else "b"
    return v.length
}

fun blockBranch(c: Boolean): Int {
    val v: String = if (c) { val t = 1; System.getenv("KRUSTY_ABSENT_18") } else "x"
    return v.length + t()
}

fun t(): Int = 0

fun conditionalArgument(c: Boolean): Int =
    takesNonNull(if (c) System.getenv("KRUSTY_ABSENT_19") else "x")

fun box(): String = "OK"
"#;

/// Each guarded position, exercised. A guard that emits but does not fire — or emits code the
/// verifier rejects — is invisible in a disassembly diff, so every position is also run.
const RUNTIME: &str = r#"
class Member {
    val memberProperty: String = System.getenv("KRUSTY_ABSENT_MEMBER")
}

fun takesNonNull(value: String): Int = value.length

fun returned(): String = System.getenv("KRUSTY_ABSENT_RETURN")

fun explicitLocal(): Int {
    val local: String = System.getenv("KRUSTY_ABSENT_LOCAL")
    return local.length
}

fun argument(): Int = takesNonNull(System.getenv("KRUSTY_ABSENT_ARGUMENT"))

fun lambdaParameter(): Int {
    val f: (String) -> Int = { it.length }
    return f(System.getenv("KRUSTY_ABSENT_LAMBDA"))
}

fun assignment(): Int {
    var v: String = "x"
    v = System.getenv("KRUSTY_ABSENT_ASSIGNMENT")
    return v.length
}

fun probe(position: Int): String = try {
    when (position) {
        0 -> returned().length
        1 -> explicitLocal()
        2 -> argument()
        3 -> lambdaParameter()
        4 -> assignment()
        else -> Member().memberProperty.length
    }
    "no assertion emitted"
} catch (e: NullPointerException) {
    e.message ?: "no message"
}

fun box(): String {
    var position = 0
    while (position < 6) {
        val message = probe(position)
        if (message != "getenv(...) must not be null") {
            return "position " + position + ": " + message
        }
        position = position + 1
    }
    return "OK"
}
"#;

/// The ticket's repro: an explicitly typed top-level property. Its guard runs in `<clinit>`, so the
/// failure surfaces as an `ExceptionInInitializerError` around the assertion's own exception.
const CLASS_INITIALIZER: &str = r#"
val absent: String = System.getenv("KRUSTY_ABSENT_CLINIT")

fun box(): String = absent
"#;

fn compile_reference(src: &str, stem: &str) -> Vec<(String, Vec<u8>)> {
    let work = common::scratch_dir().expect("allocate kotlinc call-assertion fixture");
    let source = work.join(format!("{stem}.kt"));
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, src).expect("write kotlinc call-assertion fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(code, 0, "kotlinc rejected the fixture: {stderr}");
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(!classes.is_empty(), "kotlinc emitted no classes");
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn collect_classes(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let name = path
                .strip_prefix(root)
                .expect("class below output root")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((name, std::fs::read(path).expect("read emitted class")));
        }
    }
}

/// Run a compiled fixture's `box()`.
fn run(classes: &[(String, Vec<u8>)]) -> String {
    let box_class = common::find_box_class(classes).expect("no box class emitted");
    let stdlib: Vec<PathBuf> = vec![common::stdlib_jar()];
    common::run_box(classes, &box_class, &stdlib).expect("JVM unavailable")
}

/// Every NAMED guard — `Intrinsics.checkNotNullExpressionValue` — as `(declaring method, message)`. The
/// message is the `ldc` constant the call site consumes — kotlinc derives it from the checked
/// expression (`getenv(...)`), so comparing it pins the spelling. Reading the disassembly rather
/// than the class bytes counts instructions that execute, not leftover constant-pool entries.
fn assertion_sites(classes: &[(String, Vec<u8>)], tag: &str) -> Vec<(String, String)> {
    let work = common::scratch_dir().expect("allocate javap call-assertion fixture");
    let mut arguments = vec!["-p".to_string(), "-c".to_string()];
    for (internal, bytes) in classes {
        let path = work.join(format!("{internal}.class"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create javap input directory");
        }
        std::fs::write(&path, bytes).expect("write javap input");
        arguments.push(path.to_string_lossy().into_owned());
    }
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let disassembly = common::javap(&borrowed).unwrap_or_else(|| panic!("javap failed for {tag}"));
    let _ = std::fs::remove_dir_all(work);

    let mut sites = Vec::new();
    let mut method = String::new();
    let mut last_string: Option<String> = None;
    for line in disassembly.lines() {
        let trimmed = line.trim();
        // A method header sits at javap's declaration indent and ends with `;`. A field declaration
        // does too, so require a parameter list — plus `static {};`, the class initializer that
        // carries a top-level property's guard.
        if !line.starts_with("    ")
            && trimmed.ends_with(';')
            && (trimmed.contains('(') || trimmed == "static {};")
        {
            method = trimmed.to_string();
            last_string = None;
            continue;
        }
        if trimmed.contains("ldc") {
            if let Some(constant) = trimmed.split("// String ").nth(1) {
                last_string = Some(constant.to_string());
            }
        }
        if trimmed.contains("Intrinsics.checkNotNullExpressionValue:") {
            sites.push((
                method.clone(),
                last_string
                    .clone()
                    .unwrap_or_else(|| "<no preceding constant>".to_string()),
            ));
        }
    }
    // Declaration order is a compiler's own business; which method carries which guard is not.
    sites.sort();
    sites
}

struct Builds {
    krusty: Vec<(String, Vec<u8>)>,
    reference: Vec<(String, Vec<u8>)>,
}

fn positions() -> &'static Builds {
    static BUILDS: OnceLock<Builds> = OnceLock::new();
    BUILDS.get_or_init(|| Builds {
        krusty: common::expect_classes_with_stdlib(POSITIONS, "Positions"),
        reference: compile_reference(POSITIONS, "Positions"),
    })
}

#[test]
fn every_guarded_position_fails_where_the_platform_value_enters() {
    let krusty = run(&common::expect_classes_with_stdlib(RUNTIME, "Runtime"));
    assert_eq!(
        krusty, "OK",
        "each narrowing position must throw the reference compiler's exception and message"
    );
    assert_eq!(
        krusty,
        run(&compile_reference(RUNTIME, "Runtime")),
        "the reference compiler must agree that every position fires"
    );
}

#[test]
fn a_top_level_property_fails_from_its_class_initializer() {
    let krusty = run(&common::expect_classes_with_stdlib(
        CLASS_INITIALIZER,
        "ClassInitializer",
    ));
    assert!(
        krusty.contains("NullPointerException:getenv(...) must not be null"),
        "an explicitly typed top-level property must not store null into an @NotNull field: {krusty}"
    );
    assert_eq!(
        krusty,
        run(&compile_reference(CLASS_INITIALIZER, "ClassInitializer")),
        "the reference compiler must fail the same way"
    );
}

#[test]
fn guarded_positions_match_the_reference_compiler() {
    let reference = assertion_sites(&positions().reference, "kotlinc");
    assert!(
        reference.len() >= 7,
        "the fixture must make kotlinc guard every narrowing position: {reference:?}"
    );
    assert_eq!(
        assertion_sites(&positions().krusty, "krusty"),
        reference,
        "krusty must guard the same positions with the same message as kotlinc"
    );
}

/// kotlinc's OTHER form: where the narrowed value has no name to report — a source-block branch, a
/// `try` value, the merged value of a conditional in ARGUMENT position — it emits the message-less
/// `Intrinsics.checkNotNull(Object)V` instead. krusty does not implement that form; this pins the gap
/// so it is visible (and this assertion fails) the day it is closed.
#[test]
fn the_message_less_form_is_not_implemented() {
    let nameless = |classes: &[(String, Vec<u8>)], tag| {
        let work = common::scratch_dir().expect("allocate javap fixture");
        let mut arguments = vec!["-p".to_string(), "-c".to_string()];
        for (internal, bytes) in classes {
            let path = work.join(format!("{internal}.class"));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create javap input directory");
            }
            std::fs::write(&path, bytes).expect("write javap input");
            arguments.push(path.to_string_lossy().into_owned());
        }
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let text = common::javap(&borrowed).unwrap_or_else(|| panic!("javap failed for {tag}"));
        let _ = std::fs::remove_dir_all(work);
        text.lines()
            .filter(|line| line.contains("Intrinsics.checkNotNull:(Ljava/lang/Object;)V"))
            .count()
    };
    assert!(
        nameless(&positions().reference, "kotlinc") >= 2,
        "the fixture must exercise kotlinc's message-less form"
    );
    assert_eq!(
        nameless(&positions().krusty, "krusty"),
        0,
        "krusty emits no message-less guard; `x!!` uses the same intrinsic but is absent here"
    );
}

#[test]
fn every_guarded_message_is_derived_from_the_checked_call() {
    let sites = assertion_sites(&positions().krusty, "krusty");
    assert!(!sites.is_empty(), "the fixture emitted no guard at all");
    assert!(
        sites.iter().all(|(_, message)| message == "getenv(...)"),
        "every guarded site in the fixture checks a `System.getenv` result: {sites:?}"
    );
}
