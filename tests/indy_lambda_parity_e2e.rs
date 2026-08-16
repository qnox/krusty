//! `-Xlambdas=indy` / `-Xsam-conversions=indy`: exercise the shipping CLI under the settings used by
//! intellij-community and compare its emitted class set and bootstrap table with kotlinc.

use std::path::Path;
use std::sync::OnceLock;

use super::common;

/// One Kotlin function type, one `fun interface` (a Kotlin SAM conversion), and one Java SAM
/// (`Runnable`) — the three shapes the two flags govern.
const LAMBDA_SOURCE: &str = r#"
fun interface Handler { fun handle(x: Int): Int }
fun useHandler(h: Handler): Int = h.handle(2)
fun useLambda(f: (Int) -> Int): Int = f(3)

fun box(): String {
    val a = useLambda { it * 2 }
    val b = useHandler { it * 3 }
    val r = Runnable { }
    r.run()
    return if (a == 6 && b == 6) "OK" else "fail"
}
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

fn compile_krusty() -> &'static [(String, Vec<u8>)] {
    static CLASSES: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    CLASSES
        .get_or_init(|| {
            let work = common::scratch_dir().expect("allocate krusty indy fixture");
            let source = work.join("L.kt");
            let output = work.join("out");
            std::fs::create_dir_all(&output).expect("create krusty output");
            std::fs::write(&source, LAMBDA_SOURCE).expect("write krusty indy fixture");
            let result = std::process::Command::new(common::krusty_binary())
                .args(["-d", output.to_str().expect("UTF-8 output")])
                .args(["-Xlambdas=indy", "-Xsam-conversions=indy"])
                .arg(&source)
                .output()
                .expect("run krusty CLI");
            assert!(
                result.status.success(),
                "krusty rejected indy flags: stdout={} stderr={}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let mut classes = Vec::new();
            collect_classes(&output, &output, &mut classes);
            classes.sort_by(|left, right| left.0.cmp(&right.0));
            assert!(!classes.is_empty(), "krusty emitted no classes");
            let _ = std::fs::remove_dir_all(work);
            classes
        })
        .as_slice()
}

fn compile_reference() -> &'static [(String, Vec<u8>)] {
    static CLASSES: OnceLock<Vec<(String, Vec<u8>)>> = OnceLock::new();
    CLASSES
        .get_or_init(|| {
            let work = common::scratch_dir().expect("allocate kotlinc indy fixture");
            let source = work.join("L.kt");
            let output = work.join("out");
            std::fs::create_dir_all(&output).expect("create kotlinc output");
            std::fs::write(&source, LAMBDA_SOURCE).expect("write kotlinc indy fixture");
            let args = vec![
                "-d".to_string(),
                output.to_string_lossy().into_owned(),
                "-nowarn".to_string(),
                "-Xlambdas=indy".to_string(),
                "-Xsam-conversions=indy".to_string(),
                source.to_string_lossy().into_owned(),
            ];
            let (code, stderr) =
                common::kotlinc_compile(&args).expect("reference compiler unavailable");
            assert_eq!(code, 0, "kotlinc rejected the indy fixture: {stderr}");
            let mut classes = Vec::new();
            collect_classes(&output, &output, &mut classes);
            classes.sort_by(|left, right| left.0.cmp(&right.0));
            assert!(!classes.is_empty(), "kotlinc emitted no classes");
            let _ = std::fs::remove_dir_all(work);
            classes
        })
        .as_slice()
}

fn class_names(classes: &[(String, Vec<u8>)]) -> Vec<&str> {
    classes.iter().map(|(name, _)| name.as_str()).collect()
}

fn class_bytes<'a>(classes: &'a [(String, Vec<u8>)], internal: &str) -> &'a [u8] {
    classes
        .iter()
        .find_map(|(name, bytes)| (name == internal).then_some(bytes.as_slice()))
        .unwrap_or_else(|| panic!("missing {internal}.class"))
}

/// The `BootstrapMethods` attribute of one class, with constant-pool indices erased. Stop at the
/// next top-level javap section instead of assuming this attribute will remain last forever.
fn bootstrap_methods(classes: &[(String, Vec<u8>)], internal: &str, tag: &str) -> String {
    let work = common::scratch_dir().expect("allocate javap indy fixture");
    let target = work.join(format!("{internal}.class"));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).expect("create class parent");
    }
    std::fs::write(&target, class_bytes(classes, internal)).expect("write class for javap");
    let text = common::javap(&["-v", "-p", &target.to_string_lossy()])
        .unwrap_or_else(|| panic!("pooled javap unavailable for {tag}"));
    let mut found = false;
    let mut kept = Vec::new();
    for line in text.lines() {
        if !found {
            if line == "BootstrapMethods:" {
                found = true;
                kept.push(line.to_string());
            }
            continue;
        }
        if !line.is_empty() && !line.starts_with(char::is_whitespace) {
            break;
        }
        // `#12` / `#12,  0` → `#`: allocation order is not part of the strategy.
        let mut normalized = String::new();
        let mut chars = line.chars().peekable();
        while let Some(character) = chars.next() {
            normalized.push(character);
            if character == '#' {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
        }
        kept.push(normalized.trim_end().to_string());
    }
    let _ = std::fs::remove_dir_all(work);
    kept.join("\n")
}

#[test]
fn indy_shape_matches_kotlinc() {
    let ours = compile_krusty();
    let reference = compile_reference();
    assert_eq!(
        class_names(ours),
        ["Handler", "LKt"],
        "a synthetic lambda class means the `class` strategy, not `indy`"
    );
    assert_eq!(
        class_names(ours),
        class_names(reference),
        "class set diverges under the indy flags"
    );
    assert_eq!(
        class_bytes(ours, "Handler"),
        class_bytes(reference, "Handler"),
        "the non-lambda interface class is expected to be byte-identical"
    );
    let ours_table = bootstrap_methods(ours, "LKt", "krusty indy");
    assert!(
        ours_table.contains("LambdaMetafactory"),
        "no BootstrapMethods extracted; the comparison would be vacuous: {ours_table}"
    );
    assert_eq!(
        ours_table,
        bootstrap_methods(reference, "LKt", "kotlinc indy"),
        "BootstrapMethods diverges under the indy flags"
    );
}

#[test]
fn every_lambda_becomes_an_invokedynamic_call_site() {
    let work = common::scratch_dir().expect("allocate javap call-site fixture");
    let target = work.join("LKt.class");
    std::fs::write(&target, class_bytes(compile_krusty(), "LKt")).expect("write facade class");
    let text =
        common::javap(&["-p", "-c", &target.to_string_lossy()]).expect("pooled javap unavailable");
    let sites = text
        .lines()
        .filter(|line| line.contains("invokedynamic"))
        .count();
    assert_eq!(sites, 3, "one call site per lambda in {LAMBDA_SOURCE}");
    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn indy_lambdas_execute() {
    let classes = compile_krusty();
    let box_class = common::find_box_class(classes).expect("no box class emitted");
    let result = common::run_box(classes, &box_class, &[common::stdlib_jar()])
        .expect("JVM unavailable for indy behavior test");
    assert_eq!(result, "OK");
}

#[test]
fn the_class_strategy_emits_a_different_class_set_than_indy() {
    // Both strategies compile; what distinguishes them is the artifact set, so that is the
    // assertion. (The class strategy's own behaviour is covered by `class_lambda_e2e`.)
    let mut counts = Vec::new();
    for (index, flags) in [
        ["-Xlambdas=indy", "-Xsam-conversions=indy"],
        ["-Xlambdas=class", "-Xsam-conversions=class"],
    ]
    .into_iter()
    .enumerate()
    {
        let work = common::scratch_dir().expect("allocate strategy fixture");
        let source = work.join(format!("Strategy{index}.kt"));
        let output = work.join("out");
        std::fs::write(&source, "fun value(): () -> Int = { 1 }").expect("write strategy fixture");
        let result = std::process::Command::new(common::krusty_binary())
            .args(["-d", output.to_str().expect("UTF-8 output")])
            .args(flags)
            .arg(&source)
            .output()
            .expect("run krusty CLI");
        assert!(
            result.status.success(),
            "{flags:?} rejected: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut classes = Vec::new();
        collect_classes(&output, &output, &mut classes);
        counts.push(classes.len());
        let _ = std::fs::remove_dir_all(work);
    }
    assert_eq!(
        counts[1],
        counts[0] + 1,
        "the class strategy must add exactly the lambda's own class: {counts:?}"
    );
}

#[test]
fn indy_rejects_a_pre_java_7_target_without_emitting_invalid_classes() {
    let work = common::scratch_dir().expect("allocate rejected target fixture");
    let source = work.join("RejectedTarget.kt");
    let output = work.join("out");
    std::fs::write(&source, "fun value(): () -> Int = { 1 }")
        .expect("write rejected target fixture");
    let reference_args = vec![
        "-d".to_string(),
        work.join("reference").to_string_lossy().into_owned(),
        "-jvm-target".to_string(),
        "1.6".to_string(),
        source.to_string_lossy().into_owned(),
    ];
    let (reference_code, reference_stderr) =
        common::kotlinc_compile(&reference_args).expect("reference compiler unavailable");
    assert_ne!(
        reference_code, 0,
        "kotlinc accepted the pre-Java-7 indy target: {reference_stderr}"
    );
    let result = std::process::Command::new(common::krusty_binary())
        .args([
            "-d",
            output.to_str().expect("UTF-8 output"),
            "-jvm-target",
            "1.6",
            "-Xlambdas=indy",
        ])
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert_eq!(
        result.status.code(),
        Some(1),
        "invalid indy target compiled"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("requires JVM target 1.7 or newer"),
        "the rejection did not explain the class-version constraint: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let emitted = output.exists()
        && std::fs::read_dir(&output)
            .expect("read rejected output")
            .next()
            .is_some();
    assert!(!emitted, "the rejected target emitted invalid artifacts");
    let _ = std::fs::remove_dir_all(work);
}
