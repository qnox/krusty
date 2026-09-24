//! The names of the methods lambdas and local functions are lifted into, as kotlinc's
//! `LocalDeclarationsLowering` writes them.
//!
//! A lifted method is named after the declarations around it (`measure$scale$unit`,
//! `measure$lambda$1$inner`). Lambdas and local functions whose name is already taken share one
//! numbered sequence per class and outermost declaration name, in source order: overloads share
//! it, constructors and `init` blocks share `_init_`, and a lambda spliced at an inline call site
//! still takes its number. A suspend lambda becomes a class of its own and takes none.

use super::common;

const SOURCE: &str = "fun measure(): Int {\n\
\x20   fun scale(): Int {\n\
\x20       fun unit() = 3\n\
\x20       return unit()\n\
\x20   }\n\
\x20   val first = { 4 }\n\
\x20   run {\n\
\x20       fun inner() = 5\n\
\x20       inner()\n\
\x20   }\n\
\x20   if (first() > 0) {\n\
\x20       fun pick() = 1\n\
\x20       pick()\n\
\x20   }\n\
\x20   if (first() > 0) {\n\
\x20       fun pick() = 2\n\
\x20       pick()\n\
\x20   }\n\
\x20   val last = { 6 }\n\
\x20   return scale() + first() + last()\n\
}\n\
fun measure(label: String): Int {\n\
\x20   val size = { label.length }\n\
\x20   if (size() > 0) {\n\
\x20       fun pick() = 3\n\
\x20       pick()\n\
\x20   }\n\
\x20   return size()\n\
}\n\
class Gauge(val base: Int) {\n\
\x20   val offset: () -> Int = { 10 }\n\
\x20   init {\n\
\x20       val probe = { 1 }\n\
\x20       probe()\n\
\x20   }\n\
\x20   constructor(text: String) : this(text.length) {\n\
\x20       val probe = { 2 }\n\
\x20       probe()\n\
\x20   }\n\
\x20   val reading: Int\n\
\x20       get() {\n\
\x20           fun twice(x: Int) = x * 2\n\
\x20           return twice(base)\n\
\x20       }\n\
\x20   fun read(): Int {\n\
\x20       fun pick() = base\n\
\x20       return pick()\n\
\x20   }\n\
}\n\
val total by lazy { 7 }\n\
fun holder(): Int {\n\
\x20   class Local {\n\
\x20       fun value(): Int {\n\
\x20           val v = { 8 }\n\
\x20           return v()\n\
\x20       }\n\
\x20   }\n\
\x20   val anon = object {\n\
\x20       fun value(): Int {\n\
\x20           val v = { 9 }\n\
\x20           return v()\n\
\x20       }\n\
\x20   }\n\
\x20   return Local().value() + anon.value()\n\
}\n\
class Deferred {\n\
\x20   fun values(): Int {\n\
\x20       val later: suspend () -> Int = { 1 }\n\
\x20       val now = { 2 }\n\
\x20       return now()\n\
\x20   }\n\
}\n\
fun box(): String {\n\
\x20   val sum = measure() + measure(\"ab\") + Gauge(\"abc\").offset() + Gauge(1).reading +\n\
\x20       Gauge(2).read() + total + holder() + Deferred().values()\n\
\x20   return if (sum == 55) \"OK\" else \"fail: $sum\"\n\
}\n";

/// Classes whose lifted methods match kotlinc's one for one.
const CLASSES: &[&str] = &[
    "LiftedNamesKt",
    "Gauge",
    "LiftedNamesKt$holder$Local",
    "LiftedNamesKt$holder$anon$1",
];

/// One class's lifted methods: its name, then kotlinc's and krusty's sorted declarations.
type LiftedMethods = (String, Vec<String>, Vec<String>);

/// Each class's lifted methods (private static, `$` in the name) from both compilers, as sorted
/// `javap` declarations.
fn lifted_methods(classes: &[&str]) -> Option<Vec<LiftedMethods>> {
    let dir = common::scratch_dir()?;
    let reference_dir = dir.join("ref");
    let ours_dir = dir.join("out");
    std::fs::create_dir_all(&reference_dir).ok()?;
    std::fs::create_dir_all(&ours_dir).ok()?;
    let source_path = dir.join("LiftedNames.kt");
    std::fs::write(&source_path, SOURCE).ok()?;
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        reference_dir.to_string_lossy().into_owned(),
        source_path.to_string_lossy().into_owned(),
    ])?;
    assert_eq!(code, 0, "kotlinc failed: {stderr}");
    let emitted =
        common::compile_in_process_metadata_cp(SOURCE, "LiftedNames", &[common::stdlib_jar()])
            .expect("krusty compiles the lifted callables");
    for (name, bytes) in &emitted {
        std::fs::write(ours_dir.join(format!("{name}.class")), bytes).ok()?;
    }
    let methods = |root: &std::path::Path, class: &str| {
        let dump = common::javap(&["-p", "-cp", &root.to_string_lossy(), class])?;
        let mut methods = dump
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("private static") && line.contains('('))
            .filter(|line| line.contains('$'))
            .map(str::to_string)
            .collect::<Vec<_>>();
        methods.sort();
        Some(methods)
    };
    let mut out = Vec::new();
    for class in classes {
        out.push((
            class.to_string(),
            methods(&reference_dir, class)?,
            methods(&ours_dir, class)?,
        ));
    }
    let _ = std::fs::remove_dir_all(&dir);
    Some(out)
}

#[test]
fn lifted_methods_take_kotlincs_names() {
    let Some(classes) = lifted_methods(CLASSES) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for (class, reference, ours) in classes {
        assert!(!reference.is_empty(), "{class}: kotlinc lifted something");
        assert_eq!(
            ours, reference,
            "{class}: lifted methods differ from kotlinc"
        );
    }
}

/// A suspend lambda takes no number, so the lambda after it is `values$lambda$0`. The suspend
/// lambda's own body still sits on a static here (its class is a separate mechanism), so only
/// kotlinc's methods are required.
#[test]
fn a_suspend_lambda_takes_no_number() {
    let Some(classes) = lifted_methods(&["Deferred"]) else {
        eprintln!("skipping: reference kotlinc unavailable");
        return;
    };
    for (class, reference, ours) in classes {
        assert!(!reference.is_empty(), "{class}: kotlinc lifted something");
        for method in &reference {
            assert!(
                ours.contains(method),
                "{class}: krusty lacks kotlinc's {method}; it has {ours:?}"
            );
        }
    }
}

#[test]
fn lifted_callables_run() {
    common::expect_box_same_as_kotlinc(SOURCE, "LiftedNamesRun");
}
