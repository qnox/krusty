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
//! krusty compiles Kotlin/Wasm programs in process only, under the default defect mode, so its
//! Wasm programs are checked by the Wasm lane's own test (`tests/wasm_kotlinc_defects.rs`), which
//! requires them to give what kotlinc-wasm gives. No krusty entry point compiles Kotlin/Wasm under
//! `-Xfix-kotlinc-defects` yet.

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
    /// Java sources `Main.kt` compiles against (JVM only), compiled by javac first.
    java: &'static [(&'static str, &'static str)],
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

/// KT-89451: a `null` function value converted to a nullable suspend function type becomes an
/// adapter, so the callee sees a non-null value.
const NULL_FUNCTION_VALUE_CONVERSION: &str = "import kotlin.coroutines.*\n\
    suspend fun observe(f: (suspend () -> String?)?): String = if (f == null) \"null\" else \"adapter:\" + f()\n\
    suspend fun pass(f: (() -> String?)?) = observe(f)\n\
    fun box(): String {\n\
    \x20   var answer = \"unset\"\n\
    \x20   suspend { pass(null) }.startCoroutine(Continuation(EmptyCoroutineContext) { answer = it.getOrThrow() })\n\
    \x20   return answer\n\
    }\n";

/// A `null` function value held by a type parameter with a nullable function bound, converted to a
/// nullable suspend function type: the adapter invokes it, so completing the coroutine throws.
const NULL_TYPE_PARAMETER_FUNCTION_VALUE_CONVERSION: &str = "import kotlin.coroutines.*\n\
    suspend fun observe(f: (suspend () -> String?)?): String = if (f == null) \"null\" else \"adapter:\" + f()\n\
    suspend fun <F : (() -> String?)?> pass(f: F) = observe(f)\n\
    fun box(): String {\n\
    \x20   var answer = \"unset\"\n\
    \x20   suspend { pass(null) }.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
    \x20       answer = it.fold({ r -> r }, { e -> if (e is NullPointerException) \"NPE\" else \"other\" })\n\
    \x20   })\n\
    \x20   return answer\n\
    }\n";

/// A function value held by a type parameter with a nullable function bound, passed where a
/// suspend function type is expected: the value is cast to the suspend function's class instead of
/// converted, so a lambda throws `ClassCastException`.
const TYPE_PARAMETER_FUNCTION_VALUE_CAST: &str = "import kotlin.coroutines.*\n\
    suspend fun observe(f: (suspend () -> String?)?): String = if (f == null) \"null\" else \"adapter:\" + f()\n\
    suspend fun <F : (() -> String?)?> pass(f: F) = observe(f)\n\
    fun box(): String {\n\
    \x20   var answer = \"unset\"\n\
    \x20   suspend { pass { \"y\" } }.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
    \x20       answer = it.fold({ r -> r }, { e -> if (e is ClassCastException) \"CCE\" else \"other\" })\n\
    \x20   })\n\
    \x20   return answer\n\
    }\n";

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
const LIST_ADD_ALL_OF_ITS_RANGE: &str = include_str!("kotlinc_defects/listAddAllOfItsRange.kt");

/// `JavaRenamedBuiltinEntryOverridesJavaMember`: a Java `CharSequence` whose `charAt` realizes
/// `get`, beside a `get(int)` of its own.
const BOTH_GET_AND_CHAR_AT: &str = "public class Both implements CharSequence {\n\
    \x20   public int length() { return 1; }\n\
    \x20   public char charAt(int index) { return 'c'; }\n\
    \x20   public char get(int index) { return 'g'; }\n\
    \x20   public CharSequence subSequence(int start, int end) { return \"\"; }\n\
    \x20   public String toString() { return \"Both\"; }\n\
    }\n";

/// A Kotlin subclass of [`BOTH_GET_AND_CHAR_AT`] reads the Java `get` through itself and through
/// `Both`, then the builtin `get` through `CharSequence`.
const RENAMED_BUILTIN_OVER_JAVA_MEMBER: &str = "class Inherits : Both()\n\
    fun box(): String {\n\
    \x20   val inherits = Inherits()\n\
    \x20   val both: Both = inherits\n\
    \x20   val sequence: CharSequence = inherits\n\
    \x20   return \"\" + inherits.get(0) + both.get(0) + sequence[0]\n\
    }\n";

/// A platform module's class over the class of a library its common module depends on. Compiled
/// with `-Xseparate-kmp-compilation`, kotlinc-native crashes actualizing the application's
/// fragments (KT-89466); compiled the way kotlinc's own Native box runner compiles a unit, with its
/// fragments together, it builds the program.
const PLATFORM_CLASS_OVER_LIBRARY_CLASS: &str = r#"// LANGUAGE: +MultiPlatformProjects

// MODULE: lib-common
class Some(val origin: String = "common")

object Single {
    fun origin(): String = "common"
}

// MODULE: lib-platform()()(lib-common)

// MODULE: app-common(lib-common)
fun accept(s: Some) {}

// MODULE: app-platform(lib-platform)()(app-common)
class Some(val origin: String = "platform")

object Single {
    fun origin(): String = "platform"
}

fun box(): String {
    val some: Any = Some()
    if (some !is Some) return "type test"
    if (Single.origin() != "platform") return "object"
    return if (Some().origin == "platform") "OK" else "constructor"
}
"#;

/// The program that shows `quirk`, for a defect. Every quirk is named, so a new one has to say
/// here whether it is a defect and, if so, which program shows it.
fn defect(quirk: Quirk) -> Option<Defect> {
    match quirk {
        Quirk::UnitConversionRejectsNonUnitResult => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: UNIT_CONVERSION,
            java: &[],
            with_defect: Outcome::Rejected,
            fixed: Outcome::Answer("OK".to_string()),
        }),
        Quirk::VarargReferenceTakesTypeVariableInputAsElement => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: VARARG_REFERENCE,
            java: &[],
            with_defect: Outcome::Answer("1".to_string()),
            fixed: Outcome::Answer("2".to_string()),
        }),
        Quirk::NullFunctionValueConvertsToAdapter => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: NULL_FUNCTION_VALUE_CONVERSION,
            java: &[],
            with_defect: Outcome::Answer("adapter:null".to_string()),
            fixed: Outcome::Answer("null".to_string()),
        }),
        Quirk::NullTypeParameterFunctionValueIsDereferenced => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: NULL_TYPE_PARAMETER_FUNCTION_VALUE_CONVERSION,
            java: &[],
            with_defect: Outcome::Answer("NPE".to_string()),
            fixed: Outcome::Answer("null".to_string()),
        }),
        Quirk::TypeParameterFunctionValueSkipsSuspendConversion => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: TYPE_PARAMETER_FUNCTION_VALUE_CAST,
            java: &[],
            with_defect: Outcome::Answer("CCE".to_string()),
            fixed: Outcome::Answer("adapter:y".to_string()),
        }),
        Quirk::ContextArgumentEvaluatedBeforeExtensionReceiver => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: CONTEXT_ARGUMENT_ORDER,
            java: &[],
            with_defect: Outcome::Answer("crv".to_string()),
            fixed: Outcome::Answer("rcv".to_string()),
        }),
        Quirk::ArrayListAddAllGrowsBeforeReadingItsArgument => Some(Defect {
            platforms: &[Platform::Native, Platform::WasmJs, Platform::WasmWasi],
            fixed_on: &[Platform::Jvm],
            source: LIST_ADD_ALL_OF_ITS_RANGE,
            java: &[],
            with_defect: Outcome::Answer("raised [1, 2, 3, null, null]".to_string()),
            fixed: Outcome::Answer("appended [1, 2, 3, 1, 2]".to_string()),
        }),
        Quirk::JavaRenamedBuiltinEntryOverridesJavaMember => Some(Defect {
            platforms: &[Platform::Jvm],
            fixed_on: &[],
            source: RENAMED_BUILTIN_OVER_JAVA_MEMBER,
            java: &[("Both.java", BOTH_GET_AND_CHAR_AT)],
            with_defect: Outcome::Answer("ccc".to_string()),
            fixed: Outcome::Answer("ggc".to_string()),
        }),
        Quirk::DelegateFailureNamesFirstRungOnly
        | Quirk::InheritedHolderForwarderMapsInterfaceLine
        | Quirk::SuspendUnitTailCallKeepsCalleeResult
        | Quirk::AnonymousContextParameterIsNotLocal
        | Quirk::AnonymousContextOrdinalAfterHash
        | Quirk::ContextParameterLoadMarksDeclarationLine
        | Quirk::SyntheticClassXiOmitsVisibility
        | Quirk::AnnotationImplCarriesNullability
        | Quirk::KlibLegacyMetadataVersionBelowLanguage23
        | Quirk::ClassStaticsInitializeBeforeCompanionBody
        | Quirk::NotNullOperatorDropsIntersectionApproximation
        | Quirk::InvisibleCandidatesKeepScopeLevels
        | Quirk::JvmTargetsEndAt26
        | Quirk::TailrecDotlessCallReportedAtOperator
        | Quirk::TailrecUnitObjectReturnFollowsCall
        | Quirk::TailrecElvisLeftOperandIsTail => None,
    }
}

/// Every defect, with what the reference release gives for its program.
fn defects() -> Vec<(Quirk, Defect, Outcome)> {
    let release = Quirks::of_release(krusty::kotlin_version::environment_release());
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
        for &platform in defect.platforms {
            if let Some(outcome) = kotlinc_outcome(platform, &defect) {
                assert_eq!(
                    outcome,
                    reference,
                    "{quirk:?} on {platform:?} kotlinc {}",
                    krusty::kotlin_version::environment_release()
                );
            }
        }
    }
}

#[test]
fn krusty_reproduces_each_defect_by_default() {
    for (quirk, defect, reference) in defects() {
        for &platform in defect.platforms {
            if let Some(outcome) = krusty_outcome(platform, &defect, &[]) {
                assert_eq!(outcome, reference, "{quirk:?} on {platform:?}");
            }
        }
    }
}

#[test]
fn fixing_kotlinc_defects_fixes_each_defect() {
    for (quirk, defect, _) in defects() {
        for &platform in defect.platforms {
            if let Some(outcome) = krusty_outcome(platform, &defect, &["-Xfix-kotlinc-defects"]) {
                assert_eq!(outcome, defect.fixed, "{quirk:?} on {platform:?}");
            }
        }
    }
}

/// On a platform without a library defect, kotlinc and krusty already give the fixed outcome.
#[test]
fn the_fix_is_what_the_platforms_without_the_defect_give() {
    for (quirk, defect, _) in defects() {
        for &platform in defect.fixed_on {
            for outcome in [
                kotlinc_outcome(platform, &defect),
                krusty_outcome(platform, &defect, &[]),
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
fn kotlinc_outcome(platform: Platform, defect: &Defect) -> Option<Outcome> {
    let source = defect.source;
    match platform {
        Platform::Jvm => {
            let java = java_classes(defect);
            let mut arguments = common::language_directives::kotlinc_args(source);
            if let Some(java) = &java {
                arguments.extend(["-cp".to_string(), path_text(java)]);
            }
            let (code, _) = common::reference_compiler_run(&[("Main.kt", source)], &arguments);
            Some(if code != 0 {
                Outcome::Rejected
            } else {
                Outcome::Answer(common::kotlinc_box_result_with_classpath(
                    source,
                    java.as_slice(),
                ))
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
/// `None` for Native where this host links no Native program, and for Kotlin/Wasm, which the Wasm
/// lane checks (see the module documentation).
fn krusty_outcome(platform: Platform, defect: &Defect, arguments: &[&str]) -> Option<Outcome> {
    match platform {
        Platform::Jvm => Some(krusty_jvm_outcome(defect, arguments)),
        Platform::Native => krusty_native_outcome(defect.source, arguments),
        Platform::WasmJs | Platform::WasmWasi => None,
    }
}

/// The directory javac compiles `defect`'s Java sources into, when it has any.
fn java_classes(defect: &Defect) -> Option<std::path::PathBuf> {
    if defect.java.is_empty() {
        return None;
    }
    let sources = defect
        .java
        .iter()
        .map(|(name, text)| (name.to_string(), text.to_string()))
        .collect::<Vec<_>>();
    let (classes, _) = common::javac_compile(&sources, &[]).expect("javac compiles the Java");
    Some(classes)
}

fn path_text(path: &Path) -> String {
    path.to_str().expect("UTF-8 path").to_string()
}

fn krusty_jvm_outcome(defect: &Defect, arguments: &[&str]) -> Outcome {
    let source = defect.source;
    let java = java_classes(defect);
    let work = common::scratch_dir().expect("allocate the CLI fixture");
    let path = work.join("Main.kt");
    let output = work.join("out");
    std::fs::write(&path, source).expect("write the CLI fixture");
    let result = std::process::Command::new(common::krusty_binary())
        .args(["-d", output.to_str().expect("UTF-8 output"), "-no-reflect"])
        .args(
            java.iter()
                .flat_map(|java| ["-cp".to_string(), path_text(java)]),
        )
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
    let mut classpath = vec![common::stdlib_jar()];
    classpath.extend(java);
    Outcome::Answer(common::run_box(&classes, &box_class, &classpath).expect("box() answers"))
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

/// kotlinc-native runs [`PLATFORM_CLASS_OVER_LIBRARY_CLASS`] when the oracle compiles each unit's
/// fragments together, as kotlinc's own Native box runner does: the KT-89466 crash belongs to
/// separate KMP compilation, which that runner never uses.
#[test]
fn kotlinc_native_actualizes_a_platform_class_over_a_library_class_like_its_own_runner() {
    if let Some(outcome) =
        common::kotlin_native_multimodule_outcome(PLATFORM_CLASS_OVER_LIBRARY_CLASS)
    {
        assert_eq!(
            outcome,
            common::NativeModulesOutcome::Answer("OK".to_string())
        );
    }
}
