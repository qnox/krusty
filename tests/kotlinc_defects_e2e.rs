//! kotlinc's defects ([`Quirk::is_defect`]), each shown by one program. For every defect the
//! reference release has, kotlinc still compiles the program wrongly and krusty reproduces that by
//! default; under `-Xfix-kotlinc-defects` krusty compiles it as the fixed releases do. Where the
//! reference release no longer has the defect, kotlinc and krusty agree on the fixed outcome in
//! either mode, so the table's release range is checked against kotlinc too.
//!
//! The defect mode is process-wide, so krusty compiles each program in a CLI process of its own.

use std::path::Path;

use krusty::kotlin_version::{Quirk, Quirks};

use super::common;

/// What compiling a program and running its `box()` gives.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// The compiler reports an error.
    Rejected,
    /// `box()`'s answer.
    Answer(String),
}

/// A program that a defect makes kotlinc compile wrongly.
struct Defect {
    /// One `Main.kt`, its `// LANGUAGE:` directives included.
    source: &'static str,
    /// What a release with the defect gives.
    with_defect: Outcome,
    /// What a release without it gives, and what the fix mode gives.
    fixed: Outcome,
}

/// KT-84393: a function value returning `String` passed where `() -> Unit` is expected.
const UNIT_CONVERSION: &str = "// LANGUAGE: +UnitConversionsOnArbitraryExpressions\n\
    fun consume(f: () -> Unit) { f() }\n\
    var effects = \"\"\n\
    fun produce(): String { effects += \"OK\"; return \"ignored\" }\n\
    fun pass(g: () -> String) { consume(g) }\n\
    fun box(): String { pass { produce() }; return effects }\n";

/// KT-90169: `arr.let(::countG)` passes `arr` itself, an `Array<String>`, to the vararg.
const VARARG_REFERENCE: &str = "fun <E> countG(vararg xs: E): Int = xs.size\n\
    fun box(): String { val arr = arrayOf(\"a\", \"b\"); return \"\" + arr.let(::countG) }\n";

/// `ContextArgumentEvaluatedBeforeExtensionReceiver`: a context argument written after the
/// receiver; `box()` reports the order the operands were evaluated in.
const CONTEXT_ARGUMENT_ORDER: &str = r#"// LANGUAGE: +ContextParameters +ExplicitContextArguments
var log = ""

fun logged(label: String): Int {
    log += label
    return label.length
}

fun receiver(): Double {
    log += "r"
    return 1.0
}

context(c: Int)
fun Double.contextual(value: Int): Int = c + value + toInt()

fun box(): String {
    receiver().contextual(c = logged("c"), value = logged("v"))
    return log
}
"#;

/// The program that shows `quirk`, for a defect. Every quirk is named, so a new one has to say
/// here whether it is a defect and, if so, which program shows it.
fn defect(quirk: Quirk) -> Option<Defect> {
    match quirk {
        Quirk::UnitConversionRejectsNonUnitResult => Some(Defect {
            source: UNIT_CONVERSION,
            with_defect: Outcome::Rejected,
            fixed: Outcome::Answer("OK".to_string()),
        }),
        Quirk::VarargReferenceTakesTypeVariableInputAsElement => Some(Defect {
            source: VARARG_REFERENCE,
            with_defect: Outcome::Answer("1".to_string()),
            fixed: Outcome::Answer("2".to_string()),
        }),
        Quirk::ContextArgumentEvaluatedBeforeExtensionReceiver => Some(Defect {
            source: CONTEXT_ARGUMENT_ORDER,
            with_defect: Outcome::Answer("crv".to_string()),
            fixed: Outcome::Answer("rcv".to_string()),
        }),
        Quirk::DelegateFailureNamesFirstRungOnly
        | Quirk::SuspendUnitTailCallKeepsCalleeResult
        | Quirk::AnonymousContextParameterIsNotLocal
        | Quirk::AnonymousContextOrdinalAfterHash
        | Quirk::SyntheticClassXiOmitsVisibility
        | Quirk::AnnotationImplCarriesNullability
        | Quirk::KlibLegacyMetadataVersionBelowLanguage23 => None,
    }
}

/// Every defect, with what the reference release gives for its program.
fn defects() -> Vec<(Quirk, Defect, Outcome)> {
    let release = Quirks::of_release(krusty::kotlin_version::target());
    Quirk::ALL
        .iter()
        .filter_map(|&quirk| {
            let defect = defect(quirk)?;
            let reference = if release.has(quirk) {
                defect.with_defect.clone()
            } else {
                defect.fixed.clone()
            };
            Some((quirk, defect, reference))
        })
        .collect()
}

#[test]
fn every_defect_and_only_a_defect_has_a_program() {
    for &quirk in Quirk::ALL {
        assert_eq!(defect(quirk).is_some(), quirk.is_defect(), "{quirk:?}");
    }
}

/// kotlinc gives the defect's outcome where the table says the release has it, and the fixed one
/// elsewhere.
#[test]
fn kotlinc_has_each_defect_in_the_releases_the_table_names() {
    for (quirk, defect, reference) in defects() {
        assert_eq!(
            kotlinc_outcome(defect.source),
            reference,
            "{quirk:?} on kotlinc {}",
            krusty::kotlin_version::target()
        );
    }
}

#[test]
fn krusty_reproduces_each_defect_by_default() {
    for (quirk, defect, reference) in defects() {
        assert_eq!(krusty_outcome(defect.source, &[]), reference, "{quirk:?}");
    }
}

#[test]
fn fixing_kotlinc_defects_fixes_each_defect() {
    for (quirk, defect, _) in defects() {
        assert_eq!(
            krusty_outcome(defect.source, &["-Xfix-kotlinc-defects"]),
            defect.fixed,
            "{quirk:?}"
        );
    }
}

fn kotlinc_outcome(source: &str) -> Outcome {
    let arguments = common::language_directives::kotlinc_args(source);
    let (code, _) = common::reference_compiler_run(&[("Main.kt", source)], &arguments);
    if code != 0 {
        return Outcome::Rejected;
    }
    Outcome::Answer(common::kotlinc_box_result(source))
}

/// Compile `source` with the krusty CLI under `arguments` and run its `box()`.
fn krusty_outcome(source: &str, arguments: &[&str]) -> Outcome {
    let work = common::scratch_dir().expect("allocate the CLI fixture");
    let path = work.join("Main.kt");
    let output = work.join("out");
    std::fs::write(&path, source).expect("write the CLI fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args(arguments)
        .arg(&path)
        .output()
        .expect("run the krusty CLI");
    if !result.status.success() {
        let _ = std::fs::remove_dir_all(work);
        return Outcome::Rejected;
    }
    let mut classes = Vec::new();
    collect_classes(&output, &output, &mut classes);
    let _ = std::fs::remove_dir_all(work);
    let box_class = common::find_box_class(&classes).expect("a box() class");
    Outcome::Answer(
        common::run_box(&classes, &box_class, &[common::stdlib_jar()]).expect("box() answers"),
    )
}

fn collect_classes(root: &Path, dir: &Path, classes: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).expect("read the output").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_classes(root, &path, classes);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "class")
        {
            let name = path
                .strip_prefix(root)
                .expect("under the output")
                .with_extension("")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            classes.push((name, std::fs::read(&path).expect("read a class")));
        }
    }
}
