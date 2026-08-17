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
///     constrains both its access flags and the constant kind referencing it;
///   * lambdas in a CLASS-INITIALIZATION context (a property initializer, an init-block local, a
///     direct init-block argument) — kotlinc names these from the class-init context with NO
///     function segment (`Named$h$1`, `Named$x$1`, `Named$1`), so a naming scheme that reads the
///     lexically enclosing function picks up whichever member was lowered last instead;
///   * a property and an init-block local with the SAME name (`Collide.x`) — their origin contexts
///     differ (`("x", None)` vs `("", Some("x"))`) but RENDER the same `Collide$x$…` prefix, so an
///     ordinal counter keyed by the raw context pair numbers both `$1` and one class file silently
///     overwrites the other (one lambda then computes the other's value at runtime); kotlinc
///     numbers the rendered prefix as one sequence — `Collide$x$1`, `Collide$x$2`.
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

class MultiCtor {
    val value: () -> Int = { 7 }
    constructor()
    constructor(ignored: Int)
}

class Collide {
    val x: () -> Int = { 1 }
    var q = 0
    init {
        val x = { 2 }
        q = x()
    }
}

class Named {
    val h: () -> Int = { 40 }
    var q = 0
    init {
        val x = { 9 }
        q = useLambda { it + x() }
    }
    fun member(): Int {
        val m = { 5 }
        return m()
    }
}

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
    val viaBothConstructors = MultiCtor().value() + MultiCtor(0).value()
    val named = Named()
    val viaClassInit = named.h() + named.q + named.member()
    val collide = Collide()
    val viaCollidingNames = collide.x() * 10 + collide.q
    val ok = plain == 6 && closing == 13 && sam == 6 && text == 3 &&
        real == 3.0f && pair == 33 && viaDefault == 12 && viaBothConstructors == 14 &&
        viaClassInit == 57 && viaCollidingNames == 12
    return if (ok) "OK" else "fail: $plain $closing $sam $text $real $pair $viaDefault $viaBothConstructors $viaClassInit $viaCollidingNames"
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

fn compile_krusty_with_modes_and_args(
    lambda_strategy: &str,
    sam_strategy: &str,
    extra_args: &[&str],
) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate krusty class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create krusty output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write krusty class-lambda fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args([
            &format!("-Xlambdas={lambda_strategy}"),
            &format!("-Xsam-conversions={sam_strategy}"),
        ])
        .args(extra_args)
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty rejected lambda={lambda_strategy}, sam={sam_strategy}: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    classes.sort();
    let _ = std::fs::remove_dir_all(work);
    classes
}

fn compile_krusty_with_modes(lambda_strategy: &str, sam_strategy: &str) -> Vec<String> {
    compile_krusty_with_modes_and_args(lambda_strategy, sam_strategy, &[])
}

fn compile_krusty() -> Vec<String> {
    compile_krusty_with_modes("class", "class")
}

fn compile_reference_with_modes(lambda_strategy: &str, sam_strategy: &str) -> Vec<String> {
    let work = common::scratch_dir().expect("allocate kotlinc class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write kotlinc class-lambda fixture");
    let args = vec![
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-nowarn".to_string(),
        format!("-Xlambdas={lambda_strategy}"),
        format!("-Xsam-conversions={sam_strategy}"),
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
    let ours = compile_krusty_with_modes("class", "class").len()
        - compile_krusty_with_modes("indy", "indy").len();
    let reference = compile_reference_with_modes("class", "class").len()
        - compile_reference_with_modes("indy", "indy").len();
    assert_eq!(
        ours, reference,
        "the class strategy must add the same number of classes as it does for kotlinc"
    );
}

#[test]
fn lambda_and_sam_strategies_are_independent() {
    let ours_base = compile_krusty_with_modes("indy", "indy").len();
    let reference_base = compile_reference_with_modes("indy", "indy").len();
    for (lambda_strategy, sam_strategy) in [("class", "indy"), ("indy", "class")] {
        let ours = compile_krusty_with_modes(lambda_strategy, sam_strategy).len() - ours_base;
        let reference =
            compile_reference_with_modes(lambda_strategy, sam_strategy).len() - reference_base;
        assert_eq!(
            ours, reference,
            "lambda={lambda_strategy}, sam={sam_strategy} must select only its own class strategy"
        );
    }
}

/// `-jvm-target 1.6` predates `invokedynamic` (a version-51 opcode), and avoiding it is exactly what
/// the class strategy is for — refusing the combination with an INDY error would be wrong. Assert
/// the emitted files really declare major version 50, so a "fix" that accepted the flag but emitted
/// the default version (or an indy site the old target cannot represent) still fails here.
#[test]
fn class_strategy_supports_the_pre_indy_jvm_target() {
    let work = common::scratch_dir().expect("allocate pre-indy class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create output");
    std::fs::write(&source, LAMBDA_SOURCE).expect("write fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args(["-Xlambdas=class", "-Xsam-conversions=class"])
        .args(["-jvm-target", "1.6"])
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "class-strategy lambdas do not require invokedynamic or JVM 1.7: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    assert!(
        !classes.is_empty(),
        "no classes emitted for -jvm-target 1.6"
    );
    for name in &classes {
        let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("read class");
        let major = u16::from_be_bytes([bytes[6], bytes[7]]);
        assert_eq!(major, 50, "{name} must declare class-file version 50 (1.6)");
    }
    let _ = std::fs::remove_dir_all(work);
}

/// Compare the exact class names each compiler's class strategy adds over its own indy strategy.
/// Taking the set difference excludes unrelated artifact differences such as `$DefaultImpls`.
#[test]
fn class_lambda_names_match_kotlinc() {
    let ours_class = compile_krusty();
    let ours_indy = compile_krusty_with_modes("indy", "indy");
    let reference_class = compile_reference_with_modes("class", "class");
    let reference_indy = compile_reference_with_modes("indy", "indy");
    let added = |class: Vec<String>, indy: Vec<String>| {
        class
            .into_iter()
            .filter(|name| !indy.contains(name))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        added(ours_class, ours_indy),
        added(reference_class, reference_indy),
        "lambda class names differ from kotlinc under -Xlambdas=class"
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
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
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

/// A DELEGATED property's initializer lambda (`val z by lazy { 7 }`) carries the property name, so
/// it cannot share the bare `C$<ordinal>` family with unbound init-block lambdas (where a shared
/// counter once let names collide). krusty emits `Deleg$z$1`; kotlinc emits `Deleg$z$2` — its
/// delegate ordinals count something krusty does not model yet, so this asserts krusty's own
/// deterministic set rather than a kotlinc diff (the ordinal gap is recorded in docs/SPEC.md).
#[test]
fn delegated_property_lambda_takes_the_property_name() {
    let source_text = r#"
fun use(f: () -> Int) = f()
class Deleg {
    val z: Int by lazy { 7 }
    var q = 0
    init {
        q = use { 2 }
    }
}
fun box(): String {
    val d = Deleg()
    return if (d.z == 7 && d.q == 2) "OK" else "fail: ${d.z} ${d.q}"
}
"#;
    let work = common::scratch_dir().expect("allocate delegate class-lambda fixture");
    let source = work.join("L.kt");
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create output");
    std::fs::write(&source, source_text).expect("write fixture");
    let stdlib = common::stdlib_jar();
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args(["-Xlambdas=class", "-Xsam-conversions=class"])
        .args(["-classpath", stdlib.to_str().expect("UTF-8 stdlib")])
        .arg(&source)
        .output()
        .expect("run krusty CLI");
    assert!(
        result.status.success(),
        "krusty rejected the delegate fixture: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut names = Vec::new();
    collect_classes(&output, &output, &mut names);
    let mut synthetic: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| name.contains('$'))
        .collect();
    synthetic.sort_unstable();
    assert_eq!(
        synthetic,
        ["Deleg$1", "Deleg$z$1"],
        "delegate initializer lambda must carry the property name"
    );
    let classes: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|name| {
            let bytes = std::fs::read(output.join(format!("{name}.class"))).expect("read class");
            (name.clone(), bytes)
        })
        .collect();
    let outcome = common::run_box(&classes, "LKt", &[stdlib]);
    let _ = std::fs::remove_dir_all(work);
    assert_eq!(outcome.as_deref(), Some("OK"), "delegate fixture must run");
}
