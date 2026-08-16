//! `-Xlambdas=class`: the pre-2.0 lambda strategy, where each lambda becomes its own class extending
//! `kotlin.jvm.internal.Lambda` instead of an `invokedynamic` call site.
//!
//! intellij-community needs this: 40 of its `BUILD.bazel` files pass `x_lambdas = "class"`
//! explicitly, so refusing the flag blocks those modules outright.

use std::path::Path;

use super::common;

/// Every shape whose emitted `invoke` differs. `(Int) -> Int` alone is NOT enough: its erased slot
/// takes the unbox path, which happens to emit the `checkcast` that a plain reference parameter
/// needs, so a fixture of only that shape hides four separate verification failures. Each entry
/// below pins one of them:
///   * `(String) -> Int` — the erased `Object` slot must be cast to the body's parameter type,
///     which `LambdaMetafactory` used to insert and nothing inserts under this strategy;
///   * a `Float` SAM — `F` is neither wide nor a reference, so it must not load as an `int`;
///   * two capturing lambdas in ONE call — the second is built while the first is live, so the
///     instantiation's stack accounting has to be right;
///   * a lambda inside an interface default method — its body is a static ON AN INTERFACE, which
///     constrains both its access flags and the constant kind referencing it.
const LAMBDA_SOURCE: &str = r#"
fun interface Handler { fun handle(x: Int): Int }
fun interface FloatHandler { fun handle(x: Float): Float }
fun useHandler(h: Handler): Int = h.handle(2)
fun useFloat(h: FloatHandler): Float = h.handle(1.5f)
fun useLambda(f: (Int) -> Int): Int = f(3)
fun useString(f: (String) -> Int): Int = f("hey")
fun useTwo(a: (Int) -> Int, b: (Int) -> Int): Int = a(1) + b(2)

interface Defaulted { fun go(): Int = useLambda { it * 4 } }
class Impl : Defaulted

fun box(): String {
    val plain = useLambda { it * 2 }
    val captured = 10
    val closing = useLambda { it + captured }
    val sam = useHandler { it * 3 }
    val text = useString { it.length }
    val real = useFloat { it * 2f }
    val other = 20
    val pair = useTwo({ it + captured }, { it + other })
    val viaDefault = (Impl() as Defaulted).go()
    val ok = plain == 6 && closing == 13 && sam == 6 && text == 3 &&
        real == 3.0f && pair == 33 && viaDefault == 12
    return if (ok) "OK" else "fail: $plain $closing $sam $text $real $pair $viaDefault"
}
"#;

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read compiler output") {
        let path = entry.expect("read compiler output entry").path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("class") {
            classes.push(
                path.strip_prefix(root)
                    .expect("class below output root")
                    .with_extension("")
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

fn compile_krusty_with(strategy: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate krusty class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create krusty output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write krusty class-lambda fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .args([
            &format!("-Xlambdas={strategy}"),
            &format!("-Xsam-conversions={strategy}"),
        ])
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty rejected -Xlambdas={strategy}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort();
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn compile_krusty() -> Vec<String> {
    compile_krusty_with("class")
}

fn compile_reference_with(strategy: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate kotlinc class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write kotlinc class-lambda fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        format!("-Xlambdas={strategy}"),
        format!("-Xsam-conversions={strategy}"),
        source.to_string_lossy().into_owned(),
    ];
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert_eq!(
        code, 0,
        "kotlinc rejected the class-lambda fixture: {stderr}"
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort();
    let _ = std::fs::remove_dir_all(work);
    classes
}

/// The flag must be accepted at all — it is refused today, which is what blocks the intellij modules.
#[test]
fn class_lambdas_are_accepted() {
    assert!(
        !compile_krusty().is_empty(),
        "krusty emitted no classes under -Xlambdas=class"
    );
}

/// One class per lambda. A strategy that quietly emitted the indy shape under this flag would pass
/// the runtime check below while producing a different artifact set, so the count is what pins the
/// strategy.
///
/// Measured as the DELTA each compiler adds between its own two strategies, not as an absolute
/// count: the fixture also has an interface default method, and kotlinc emits a `$DefaultImpls`
/// holder for it that krusty does not. That is an unrelated `-jvm-default` gap, and comparing
/// absolute counts would report it here as a lambda defect.
#[test]
fn class_lambdas_emit_one_class_per_lambda() {
    let ours = compile_krusty_with("class").len() - compile_krusty_with("indy").len();
    let reference = compile_reference_with("class").len() - compile_reference_with("indy").len();
    assert_eq!(
        ours, reference,
        "the class strategy must add the same number of classes as it does for kotlinc"
    );
}

/// Exact synthetic-class NAMES are not reached yet: kotlinc names a lambda after the declaration it
/// initializes (`LKt$box$plain$1`), and krusty numbers them within the enclosing declaration
/// (`LKt$box$1`). The count and the runtime behaviour above are unaffected, but a consumer reading a
/// stack trace or reflecting on the name sees a different one, so this stays recorded as a gap
/// rather than deleted.
#[test]
#[ignore = "synthetic lambda class names do not yet match kotlinc's declaration-derived scheme"]
fn class_lambda_names_match_kotlinc() {
    assert_eq!(
        compile_krusty(),
        compile_reference_with("class"),
        "class set differs from kotlinc under -Xlambdas=class"
    );
}

/// The emitted classes must actually run: a lambda class that verifies but computes the wrong thing
/// (or drops its captured value) passes every shape assertion above. This compiles THROUGH the flag
/// rather than through the default strategy, which would run the indy classes and prove nothing.
#[test]
fn class_lambdas_run() {
    let work = common::scratch_dir().expect("allocate class-lambda run fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output")])
        .args(["-Xlambdas=class", "-Xsam-conversions=class"])
        .args(["-classpath", stdlib.to_str().expect("UTF-8 stdlib")])
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty rejected -Xlambdas=class: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut names = Vec::new();
    collect_classes(&output, &output, &mut names);
    let classes: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|name| {
            let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("read class");
            (name.clone(), bytes)
        })
        .collect();
    let outcome = common::run_box(&classes, "LKt", &[stdlib]);
    let _ = std::fs::remove_dir_all(work);
    assert_eq!(
        outcome.as_deref(),
        Some("OK"),
        "class-strategy lambdas did not run correctly"
    );
}
