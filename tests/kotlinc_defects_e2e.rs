//! kotlinc's defects ([`Quirk::is_defect`]), each shown by one program. For every defect the
//! reference release has, kotlinc still compiles the program wrongly and krusty reproduces that by
//! default; under `-Xfix-kotlinc-defects` krusty compiles it as the fixed releases do. Where the
//! reference release no longer has the defect, kotlinc and krusty agree on the fixed outcome in
//! either mode, so the table's release range is checked against kotlinc too.
//!
//! A library defect belongs to the platforms whose standard library has it; on another platform
//! the program already gives the fixed outcome, which is what the fix mode gives on the defect's.
//!
//! The defect mode is process-wide, so krusty compiles each program in a CLI process of its own.
//! kotlinc-native answers from its recording when its distribution is not provisioned, and a Native
//! check runs where the host links Native programs, as every Native test does. A Kotlin/Wasm
//! program runs under the exact kotlinc-wasm and Node.js.
//!
//! GAP: krusty's Wasm runtime lays out its own `ArrayList`, which reads `addAll`'s argument before it
//! grows, so krusty's Wasm programs are not checked here yet. They will be once the target stdlib's
//! `ArrayList` body replaces that runtime list (#3836).

use std::path::Path;

use krusty::compilation_target::CompilationTarget;
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

/// The platform a program is compiled for.
#[derive(Clone, Copy, Debug)]
enum Platform {
    Jvm,
    Native,
    WasmJs,
    WasmWasi,
}

/// A program that a defect makes kotlinc compile wrongly.
struct Defect {
    /// The platforms whose kotlinc has the defect.
    platforms: &'static [Platform],
    /// Platforms whose own kotlinc and library already give the fixed outcome.
    fixed_on: &'static [Platform],
    /// One `Main.kt`, its `// LANGUAGE:` directives included.
    source: &'static str,
    /// What a release with the defect gives.
    with_defect: Outcome,
    /// What a release without it gives, and what the fix mode gives.
    fixed: Outcome,
    /// What a release that has another defect gives instead of `fixed`, when that defect rejects
    /// the program before this one shows.
    masked_by: &'static [(Quirk, Outcome)],
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

/// `ArrayListAddAllGrowsBeforeReadingItsArgument`: a list appends a range of itself.
const LIST_ADD_ALL_OF_ITS_RANGE: &str = "fun box(): String {\n\
    \x20   val list = ArrayList<Int>()\n\
    \x20   list.add(1)\n\
    \x20   list.add(2)\n\
    \x20   list.add(3)\n\
    \x20   val answer = try {\n\
    \x20       list.addAll((list as List<Int>).subList(0, 2))\n\
    \x20       \"appended\"\n\
    \x20   } catch (e: ConcurrentModificationException) {\n\
    \x20       \"raised\"\n\
    \x20   }\n\
    \x20   return \"$answer $list\"\n\
    }\n";

/// `InterfaceBlockPropertyInitializerProhibited`: a constant of an interface's `companion { … }`
/// block, whose initializer the first 2.4 releases take for an interface property's.
const INTERFACE_BLOCK_CONSTANT: &str = "// LANGUAGE: +CompanionBlocks\n\
    interface Source {\n\
    \x20   companion {\n\
    \x20       const val fixed: String = \"OK\"\n\
    \x20   }\n\
    }\n\
    fun box(): String = Source.fixed\n";

/// `InterfaceBlockPropertyFieldIsPrivateStatic`: a stored property of an interface's
/// `companion { … }` block, which the JVM only admits in a class of its own.
const INTERFACE_BLOCK_STORED_PROPERTY: &str = "// LANGUAGE: +CompanionBlocks\n\
    interface Source {\n\
    \x20   companion {\n\
    \x20       var current: String = \"O\"\n\
    \x20       val fixed: String = \"K\"\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   Source.current += \"\"\n\
    \x20   return Source.current + Source.fixed\n\
    }\n";

/// The program that shows `quirk`, for a defect. Every quirk is named, so a new one has to say
/// here whether it is a defect and, if so, which program shows it.
fn defect(quirk: Quirk) -> Option<Defect> {
    match quirk {
        Quirk::UnitConversionRejectsNonUnitResult => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: UNIT_CONVERSION,
            with_defect: Outcome::Rejected,
            fixed: Outcome::Answer("OK".to_string()),
            masked_by: &[],
        }),
        Quirk::VarargReferenceTakesTypeVariableInputAsElement => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: VARARG_REFERENCE,
            with_defect: Outcome::Answer("1".to_string()),
            fixed: Outcome::Answer("2".to_string()),
            masked_by: &[],
        }),
        Quirk::ContextArgumentEvaluatedBeforeExtensionReceiver => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: CONTEXT_ARGUMENT_ORDER,
            with_defect: Outcome::Answer("crv".to_string()),
            fixed: Outcome::Answer("rcv".to_string()),
            masked_by: &[],
        }),
        Quirk::ArrayListAddAllGrowsBeforeReadingItsArgument => Some(Defect {
            platforms: &[Platform::Native, Platform::WasmJs, Platform::WasmWasi],
            fixed_on: &[Platform::Jvm],
            source: LIST_ADD_ALL_OF_ITS_RANGE,
            with_defect: Outcome::Answer("raised [1, 2, 3, null, null]".to_string()),
            fixed: Outcome::Answer("appended [1, 2, 3, 1, 2]".to_string()),
            masked_by: &[],
        }),
        Quirk::InterfaceBlockPropertyInitializerProhibited => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: INTERFACE_BLOCK_CONSTANT,
            with_defect: Outcome::Rejected,
            fixed: Outcome::Answer("OK".to_string()),
            masked_by: &[],
        }),
        Quirk::InterfaceBlockPropertyFieldIsPrivateStatic => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: INTERFACE_BLOCK_STORED_PROPERTY,
            with_defect: Outcome::Answer(
                "ERROR:ClassFormatError:Illegal field modifiers in class Source: 0xA".to_string(),
            ),
            fixed: Outcome::Answer("OK".to_string()),
            masked_by: &[(
                Quirk::InterfaceBlockPropertyInitializerProhibited,
                Outcome::Rejected,
            )],
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
                defect
                    .masked_by
                    .iter()
                    .find(|(masking, _)| release.has(*masking))
                    .map_or_else(|| defect.fixed.clone(), |(_, outcome)| outcome.clone())
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
        for &platform in defect.platforms {
            if let Some(outcome) = kotlinc_outcome(platform, defect.source) {
                assert_eq!(
                    outcome,
                    reference,
                    "{quirk:?} on {platform:?} kotlinc {}",
                    krusty::kotlin_version::target()
                );
            }
        }
    }
}

#[test]
fn krusty_reproduces_each_defect_by_default() {
    for (quirk, defect, reference) in defects() {
        for &platform in defect.platforms {
            if let Some(outcome) = krusty_outcome(platform, defect.source, &[]) {
                assert_eq!(outcome, reference, "{quirk:?} on {platform:?}");
            }
        }
    }
}

#[test]
fn fixing_kotlinc_defects_fixes_each_defect() {
    for (quirk, defect, _) in defects() {
        for &platform in defect.platforms {
            if let Some(outcome) =
                krusty_outcome(platform, defect.source, &["-Xfix-kotlinc-defects"])
            {
                assert_eq!(outcome, defect.fixed, "{quirk:?} on {platform:?}");
            }
        }
    }
}

/// The initializer diagnostics a release with `InterfaceBlockPropertyInitializerProhibited` gives
/// the stored property program: one per initializer, in source order, and none elsewhere. kotlinc
/// and krusty agree on every file, line, column and message.
#[test]
fn the_prohibited_initializers_are_reported_as_kotlinc_reports_them() {
    let source = INTERFACE_BLOCK_STORED_PROPERTY;
    let diagnostic = |line, column| common::CompilerError {
        file: "Main.kt".to_string(),
        line,
        column,
        message: "property initializers in interfaces are prohibited.".to_string(),
    };
    let expected = if Quirks::of_release(krusty::kotlin_version::target())
        .has(Quirk::InterfaceBlockPropertyInitializerProhibited)
    {
        vec![diagnostic(4, 31), diagnostic(5, 29)]
    } else {
        Vec::new()
    };
    let result = common::compiler_diagnostics_with_reference_args(
        &[("Main.kt", source)],
        &[common::stdlib_jar()],
        &common::language_directives::kotlinc_args(source),
    );
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
}

/// On a platform without a library defect, kotlinc and krusty already give the fixed outcome.
#[test]
fn the_fix_is_what_the_platforms_without_the_defect_give() {
    for (quirk, defect, _) in defects() {
        for &platform in defect.fixed_on {
            for outcome in [
                kotlinc_outcome(platform, defect.source),
                krusty_outcome(platform, defect.source, &[]),
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(outcome, defect.fixed, "{quirk:?} on {platform:?}");
            }
        }
    }
}

/// kotlinc's outcome for `source` on `platform`; `None` where kotlinc-native is neither provisioned
/// nor recorded, or the kotlinc distribution has no Kotlin/Wasm compiler.
fn kotlinc_outcome(platform: Platform, source: &str) -> Option<Outcome> {
    match platform {
        Platform::Jvm => {
            let arguments = common::language_directives::kotlinc_args(source);
            let (code, _) = common::reference_compiler_run(&[("Main.kt", source)], &arguments);
            Some(if code != 0 {
                Outcome::Rejected
            } else {
                Outcome::Answer(common::kotlinc_box_result(source))
            })
        }
        Platform::Native => common::kotlin_native_box(source).map(Outcome::Answer),
        Platform::WasmJs => kotlinc_wasm_outcome(CompilationTarget::WasmJs, source),
        Platform::WasmWasi => kotlinc_wasm_outcome(CompilationTarget::WasmWasi, source),
    }
}

fn kotlinc_wasm_outcome(target: CompilationTarget, source: &str) -> Option<Outcome> {
    let work = common::scratch_dir().expect("allocate the Kotlin/Wasm fixture");
    let path = work.join("box.kt");
    std::fs::write(&path, format!("{source}\nfun main() {{ print(box()) }}\n"))
        .expect("write the Kotlin/Wasm fixture");
    let run = crate::reference_wasm_program::run_wasm_reference_program(target, &work, &[path]);
    let _ = std::fs::remove_dir_all(&work);
    Some(match run? {
        Ok(answer) => Outcome::Answer(answer.trim_end_matches('\n').to_string()),
        Err(error) if error.starts_with("reference KLIB:") => Outcome::Rejected,
        Err(error) => panic!("kotlinc {target:?}: {error}"),
    })
}

/// Compile `source` for `platform` with the krusty CLI under `arguments` and run its `box()`;
/// `None` for Native where this host links no Native program, and for Kotlin/Wasm (the GAP in the
/// module documentation).
fn krusty_outcome(platform: Platform, source: &str, arguments: &[&str]) -> Option<Outcome> {
    match platform {
        Platform::Jvm => Some(krusty_jvm_outcome(source, arguments)),
        Platform::Native => krusty_native_outcome(source, arguments),
        Platform::WasmJs | Platform::WasmWasi => None,
    }
}

fn krusty_jvm_outcome(source: &str, arguments: &[&str]) -> Outcome {
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

fn krusty_native_outcome(source: &str, arguments: &[&str]) -> Option<Outcome> {
    let target = krusty_native::NativeTarget::host()?;
    if !krusty_native::can_link(target) {
        return None;
    }
    let work = common::scratch_dir().expect("allocate the Native fixture");
    let path = work.join("box.kt");
    std::fs::write(&path, format!("{source}\nfun main() {{ print(box()) }}\n"))
        .expect("write the Native fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .arg("kotlinc-native")
        .args(arguments)
        .arg(&path)
        .arg("-o")
        .arg(work.join("program"))
        .output()
        .expect("run krusty kotlinc-native");
    if !result.status.success() {
        let _ = std::fs::remove_dir_all(&work);
        return Some(Outcome::Rejected);
    }
    let run = std::process::Command::new(work.join("program.kexe"))
        .output()
        .expect("run the krusty Native program");
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        run.status.success(),
        "the krusty Native program exited with {}:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    Some(Outcome::Answer(
        String::from_utf8(run.stdout).expect("box() answers text"),
    ))
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
