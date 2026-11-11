//! The `codegen/box` corpus through krusty's WebAssembly backend, once per kotlinc Wasm target.
//!
//! Each case is compiled into one WasmGC module whose `_start` prints `box()`'s answer, plus the
//! `.mjs` loader that runs it, and RUN under Node.js: `wasm-js` through the loader's JavaScript
//! imports, `wasm-wasi` through Node's WASI preview-1 host. The driver, the verdict and the
//! committed ratchet are every runnable target's (`box_lane`); this file is what is Wasm's.
//!
//! The two targets are separate lanes with separate inventories because the corpus treats them
//! separately — `// IGNORE_BACKEND: WASM_WASI` mutes a case on one only — and because the host
//! boundary that separates them is exactly where they can disagree.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use krusty::backend::Entry;
use krusty::conformance::TestTarget;
use krusty_wasm::{WasmBackend, WasmTarget};

use super::box_lane::{self, Lane, ModuleCase, Outcome};
use super::box_ratchet::Platform;

/// The prefix the wasm backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the wasm backend does not support ";

/// The oldest Node.js whose WebAssembly runs WasmGC modules without flags.
const MINIMUM_NODE: u32 = 22;

struct WasmLane {
    target: WasmTarget,
}

impl WasmLane {
    /// The directive tokens that name this target: `WASM` names both.
    fn tokens(&self) -> [&'static str; 2] {
        ["WASM", self.target.backend_token()]
    }

    fn backend(&self, source: &str) -> Result<WasmBackend, Outcome> {
        let options = krusty::conformance::WasmCompilerOptions::from_source(source)
            .map_err(Outcome::Harness)?;
        let mut backend = WasmBackend::new(self.target).with_entry(Entry::Box);
        if let Some(enabled) = options.array_range_checks() {
            backend = backend.with_array_range_checks(enabled);
        }
        Ok(backend)
    }

    fn run_artifacts(
        &self,
        scratch: &Path,
        stem: &str,
        suffix: &str,
        program: box_lane::CompiledProgram,
    ) -> Outcome {
        if !program.native_imports.is_empty() {
            return Outcome::Harness("a Wasm compilation produced Native link inputs".to_string());
        }
        let artifacts = program.artifacts;
        if artifacts.is_empty() {
            return Outcome::Frontend(
                "the frontend produced no checked file and said nothing".to_string(),
            );
        }
        let directory = scratch.join(format!(
            "{stem}-{suffix}-{}-{}-{}",
            self.target.name(),
            std::process::id(),
            rayon::current_thread_index().unwrap_or(0)
        ));
        if let Err(error) = std::fs::create_dir_all(&directory) {
            return Outcome::Harness(format!("creating the case directory: {error}"));
        }
        for (name, bytes) in &artifacts {
            if let Err(error) = std::fs::write(directory.join(name), bytes) {
                return Outcome::Harness(format!("writing {name}: {error}"));
            }
        }
        let outcome = box_lane::run_program(
            Command::new("node")
                .arg("--no-warnings")
                .arg(directory.join("box.mjs")),
        );
        let _ = std::fs::remove_dir_all(&directory);
        outcome
    }
}

/// Why this host has no Node.js able to run the modules, or `None` when it has one.
fn node_unavailable() -> Option<String> {
    static NODE: OnceLock<Option<String>> = OnceLock::new();
    NODE.get_or_init(|| {
        let output = match Command::new("node").arg("--version").output() {
            Ok(output) if output.status.success() => output,
            _ => return Some("no `node` on PATH".to_string()),
        };
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let major = version
            .trim_start_matches('v')
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok());
        match major {
            Some(major) if major >= MINIMUM_NODE => None,
            _ => Some(format!(
                "node {version} predates WasmGC support (needs {MINIMUM_NODE} or newer)"
            )),
        }
    })
    .clone()
}

impl Lane for WasmLane {
    fn platform(&self) -> Platform {
        match self.target {
            WasmTarget::Js => Platform::WasmJs,
            WasmTarget::Wasi => Platform::WasmWasi,
        }
    }

    fn label(&self) -> &'static str {
        self.target.name()
    }

    fn env_infix(&self) -> &'static str {
        self.target.backend_token()
    }

    fn test_target(&self) -> TestTarget {
        match self.target {
            WasmTarget::Js => TestTarget::WasmJs,
            WasmTarget::Wasi => TestTarget::WasmWasi,
        }
    }

    fn unavailable(&self) -> Option<String> {
        if !box_lane::stdlib_klib_available(self.test_target()) {
            return Some("no stdlib and JDK modules to analyze against".to_string());
        }
        node_unavailable()
    }

    fn not_applicable(&self, src: &str) -> Option<&'static str> {
        if !krusty::conformance::backend_targeted(src, &self.tokens()) {
            return Some(box_lane::TARGETED_AT_ANOTHER_BACKEND);
        }
        if krusty::conformance::backend_muted(src, &self.tokens()) {
            return Some("muted on this wasm target");
        }
        if krusty::conformance::directive(src, "FULL_JDK") {
            return Some("requires the JDK runtime");
        }
        None
    }

    fn harness_limitation(&self, _src: &str) -> Option<&'static str> {
        None
    }

    fn compile_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        sources: &[(String, String)],
        directive_source: &str,
    ) -> Outcome {
        let backend = match self.backend(directive_source) {
            Ok(backend) => backend,
            Err(outcome) => return outcome,
        };
        let program = match box_lane::compile(
            sources,
            directive_source,
            self.test_target(),
            &backend,
            "box",
            DECLINE_PREFIX,
        ) {
            Ok(program) => program,
            Err(outcome) => return outcome,
        };
        self.run_artifacts(scratch, stem, "single", program)
    }

    fn compile_modules_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        modules: &[krusty::conformance::ModuleUnit],
        directive_source: &str,
    ) -> Outcome {
        let Some(entry_module) = modules.last() else {
            return Outcome::Harness("a module graph has no final source module".to_string());
        };
        let backend = match self.backend(directive_source) {
            Ok(backend) => backend.with_entry_module(entry_module.name.clone()),
            Err(outcome) => return outcome,
        };
        let features = krusty::features::LangFeatures::from_source(directive_source);
        let platform = match self.target {
            WasmTarget::Js => krusty::klib::write::KlibPlatform::WasmJs,
            WasmTarget::Wasi => krusty::klib::write::KlibPlatform::WasmWasi,
        };
        let metadata = krusty::klib::backend::KlibBackend::new(
            platform,
            krusty::klib::write::KlibStamp::new(
                krusty::kotlin_version::target(),
                features.language_version(),
            ),
        )
        .with_annotations_in_metadata(features.has("AnnotationsInMetadata"));
        let program = match box_lane::compile_modules(
            ModuleCase {
                scratch,
                stem,
                modules,
            },
            directive_source,
            self.test_target(),
            &backend,
            &metadata,
            DECLINE_PREFIX,
        ) {
            Ok(program) => program,
            Err(outcome) => return outcome,
        };
        self.run_artifacts(scratch, stem, "modules", program)
    }
}

#[test]
fn wasm_applicability_reads_both_the_family_and_the_exact_target() {
    let js = WasmLane {
        target: WasmTarget::Js,
    };
    let wasi = WasmLane {
        target: WasmTarget::Wasi,
    };
    assert_eq!(js.test_target(), TestTarget::WasmJs);
    assert_eq!(wasi.test_target(), TestTarget::WasmWasi);
    for (target, expected) in [
        (TestTarget::WasmJs, "kotlin-stdlib-wasm-js.klib".to_string()),
        (
            TestTarget::WasmWasi,
            format!(
                "kotlin-stdlib-wasm-wasi-{}.klib",
                krusty::kotlin_version::target()
            ),
        ),
    ] {
        if let Some(path) = krusty::toolchain::kotlin_stdlib_klib(target.compilation_target()) {
            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some(expected.as_str())
            );
        }
    }
    let muted_on_wasi = "// IGNORE_BACKEND: WASM_WASI\nfun box() = \"OK\"";
    assert_eq!(js.not_applicable(muted_on_wasi), None);
    assert_eq!(
        wasi.not_applicable(muted_on_wasi),
        Some("muted on this wasm target")
    );
    let wasm_only = "// TARGET_BACKEND: WASM\nfun box() = \"OK\"";
    assert_eq!(js.not_applicable(wasm_only), None);
    assert_eq!(wasi.not_applicable(wasm_only), None);
    assert_eq!(
        js.not_applicable("// TARGET_BACKEND: JVM\nfun box() = \"OK\""),
        Some(box_lane::TARGETED_AT_ANOTHER_BACKEND)
    );
    assert_eq!(
        js.not_applicable("// IGNORE_BACKEND: JVM\nfun box() = \"OK\""),
        None,
        "a JVM mute must not remove Wasm coverage"
    );
}

#[test]
fn wasm_unsigned_range_membership_orders_high_bits_as_values() {
    expect_wasm_box(
        r#"
fun box(): String {
    if (0x80000000u !in 0x7fffffffu..0xffffffffu) return "uint inside"
    if (1u in 0x80000000u..0xffffffffu) return "uint outside"
    if (0x8000000000000000uL !in 0x7fffffffffffffffuL..0xffffffffffffffffuL) return "ulong inside"
    if (1uL in 0x8000000000000000uL..0xffffffffffffffffuL) return "ulong outside"
    return "OK"
}
"#,
        "wasmUnsignedRangeMembership",
    );
}

#[test]
fn wasm_unsigned_equality_and_shifts_use_the_checked_unsigned_carrier() {
    expect_wasm_box(
        r#"
fun box(): String {
    if (0x80000000u != 0x80000000u) return "uint equality"
    if (0x8000000000000000uL != 0x8000000000000000uL) return "ulong equality"
    if (1u shl 20 != 1_048_576u) return "uint shl"
    if (1uL shl 20 != 1_048_576uL) return "ulong shl"
    if (1u shl -1 != 0x80000000u || 1u shl 32 != 1u || 1u shl 33 != 2u) return "uint mask"
    if (1uL shl -1 != 0x8000000000000000uL || 1uL shl 64 != 1uL || 1uL shl 65 != 2uL) return "ulong mask"
    if (0x80000000u shr 31 != 1u) return "uint shr"
    if (0x8000000000000000uL shr 63 != 1uL) return "ulong shr"
    return "OK"
}
"#,
        "wasmUnsignedEqualityAndShifts",
    );
}

#[test]
fn wasm_mixed_numeric_range_contains_uses_the_selected_stdlib_declaration() {
    expect_wasm_box(
        r#"
fun box(): String {
    if (1L !in 1..2 || 0L in 1..2) return "long in int"
    if (1 !in 1L..2L || 0 in 1L..2L) return "int in long"
    if (1.0 !in 1.0f..2.0f || 0.0 in 1.0f..2.0f) return "double in float"
    if (1.0f !in 1.0..2.0 || 0.0f in 1.0..2.0) return "float in double"
    return "OK"
}
"#,
        "wasmMixedNumericRangeContains",
    );
}

#[test]
fn wasm_reinterpretation_runs_through_emitted_stdlib_extension_bodies() {
    expect_wasm_box(
        r#"
fun box(): String {
    if (0.0f.toRawBits() != 0 || (-0.0f).toRawBits() != Int.MIN_VALUE) return "float bits"
    if (Float.fromBits(Int.MIN_VALUE).toRawBits() != Int.MIN_VALUE) return "float value"
    if (0.0.toRawBits() != 0L || (-0.0).toRawBits() != Long.MIN_VALUE) return "double bits"
    if (Double.fromBits(Long.MIN_VALUE).toRawBits() != Long.MIN_VALUE) return "double value"
    return "OK"
}
"#,
        "wasmReinterpretationThroughStdlibBodies",
    );
}

#[test]
fn wasm_bit_counts_run_as_the_stdlib_declared_instructions() {
    expect_wasm_box(
        r#"
fun box(): String {
    if (0x00f0.countLeadingZeroBits() != 24) return "clz"
    if (0x0100.countTrailingZeroBits() != 8) return "ctz"
    if ((-1).countOneBits() != 32 || 0.countOneBits() != 0) return "popcnt"
    if ((1L shl 40).countLeadingZeroBits() != 23) return "long clz"
    return "OK"
}
"#,
        "wasmBitCountInstructions",
    );
}

#[test]
fn wasm_narrowing_numeric_conversions_wrap_and_saturate() {
    expect_wasm_box(
        r#"
fun box(): String {
    val big = 0x1_0000_0005L
    if (big.toInt() != 5 || (-1L).toInt() != -1) return "Long.toInt"
    if (0x1ffL.toByte() != (-1).toByte() || 0x18000L.toShort() != Short.MIN_VALUE) return "Long narrow"
    val nan = 0.0 / 0.0
    if (nan.toInt() != 0 || 1e300.toInt() != Int.MAX_VALUE || (-1e300).toInt() != Int.MIN_VALUE) return "Double.toInt"
    if ((-2.9).toLong() != -2L || 1e30f.toLong() != Long.MAX_VALUE) return "toLong"
    if (2.9f.toInt() != 2) return "Float.toInt"
    if (1.5.toFloat() != 1.5f) return "Double.toFloat"
    return "OK"
}
"#,
        "wasmNarrowingNumericConversions",
    );
}

#[test]
fn wasm_math_runs_through_emitted_stdlib_instruction_wrappers() {
    expect_wasm_box(
        r#"
import kotlin.math.*

fun box(): String {
    if (sqrt(16.0) != 4.0 || sqrt(2.25f) != 1.5f) return "sqrt"
    if (floor(-1.5) != -2.0 || ceil(-1.5) != -1.0 || truncate(-1.5) != -1.0) return "double rounding"
    if (floor(-1.5f) != -2f || ceil(-1.5f) != -1f || truncate(-1.5f) != -1f) return "float rounding"
    if (abs(-3.0) != 3.0 || abs(-3f) != 3f) return "abs"
    if (1.0 / min(0.0, -0.0) != Double.NEGATIVE_INFINITY || max(2.0, 3.0) != 3.0) return "min max"
    val nan = min(Double.NaN, 1.0)
    if (nan == nan) return "min propagates NaN"
    if (max(1f, 2f) != 2f || min(1f, 2f) != 1f) return "float min max"
    if (2.0.withSign(-1.0) != -2.0 || 2f.withSign(-1f) != -2f) return "withSign"
    return "OK"
}
"#,
        "wasmMathInstructionWrappers",
    );
}

#[test]
fn wasm_a_scope_function_lambda_mutates_a_caller_var_through_its_shared_cell() {
    expect_wasm_box(
        r#"
fun box(): String {
    var count = 0
    var last = "none"
    val size = "abc".let {
        count += it.length
        last = it
        it.length
    }
    run { count++ }
    with("de") { count += length }
    if (size != 3) return "result $size"
    if (count != 6) return "count $count"
    if (last != "abc") return "last $last"
    return "OK"
}
"#,
        "wasmScopeFunctionSharedCell",
    );
}

/// Two function values of one property reference, made at different sites, are equal: the
/// reference is identified by the property it reads, not by the adapter each site allocates.
#[test]
fn wasm_property_reference_function_values_are_equal_across_sites() {
    const SOURCE: &str = include_str!("box_regressions/propertyReferenceFunctionValueIdentity.kt");
    expect_reference_box(SOURCE, "wasmPropertyReferenceEquality");
    expect_wasm_box(SOURCE, "wasmPropertyReferenceEquality");
}

/// A file that references a property declared in a later file reads it through the checked
/// property, and its references equal the declaring file's.
#[test]
fn wasm_property_reference_function_values_are_equal_across_files() {
    const SOURCE: &str = include_str!("box_regressions/propertyReferenceIdentityAcrossFiles.kt");
    const STEM: &str = "wasmPropertyReferenceIdentityAcrossFiles";
    super::box_reference_classes::checked_box(SOURCE, STEM)
        .expect("the exact Kotlin compiler must run the whole fixture");
    let (files, java) = krusty::conformance::split_files(SOURCE);
    assert!(java.is_empty());
    let files = files
        .into_iter()
        .map(|(name, content)| (format!("{name}.kt"), content))
        .collect::<Vec<_>>();
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(&lane, STEM));
        assert_eq!(
            lane.compile_and_run(&std::env::temp_dir(), STEM, &files, SOURCE),
            Outcome::Pass,
            "{STEM} on {}",
            target.name(),
        );
    }
}

/// What a constructor that reaches back into its own enum finds, as kotlinc-wasm builds an enum:
/// each constant is published once constructed, and a constant with a body right after its super
/// call, so its own body already finds it. The expected log is kotlinc-wasm's for this program
/// (2.4.21; wasm-js and wasm-wasi agree), taken by running it there. Kotlin/Native publishes every
/// constant before constructing any (`native_enum_publication_e2e`), so this is target-owned.
#[test]
fn wasm_enum_constants_are_published_once_constructed() {
    expect_wasm_box(
        r##"
var log = ""

fun describe(x: Any?, self: Any): String = when {
    x == null -> "null"
    x === self -> "self"
    else -> "other"
}

fun seen(self: Any): String =
    "[" + describe(E.C, self) + describe(E.A, self) + describe(E.B, self) + "]"

enum class E(val tag: Int) {
    C(3),
    A(1),
    B(2) {
        init { log += "body" + seen(this) }
    };

    init { log += ordinal.toString() + seen(this) }
}

fun box(): String {
    E.A
    val values = E.values()
    val missing = try {
        E.valueOf("D").name
    } catch (e: Throwable) {
        e.message
    }
    val result = log + "#" + values[0].name + values[1].name + values[2].name + "#" + missing
    val expected = "0[nullnullnull]1[othernullnull]2[otherothernull]body[otherotherself]#CAB#No enum constant E.D"
    return if (result == expected) "OK" else result
}
"##,
        "wasmEnumPublication",
    );
}

/// A same-module `@InlineOnly` wrapper is never emitted, so its function-typed parameter reaches no
/// Wasm signature while its lambda expands through common IR. The lambda's ordinary assignments to
/// a captured `var` are checked as `Unit` statements and must leave nothing on the stack: inside an
/// `if` branch of the lambda, a leftover value would not validate.
#[test]
fn wasm_a_same_module_inline_only_lambda_assigns_a_captured_var_as_a_unit_statement() {
    expect_wasm_box(
        r#"
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")

@kotlin.internal.InlineOnly
inline fun twice(block: () -> Unit) {
    block()
    block()
}

fun box(): String {
    var count = 0
    var last = "none"
    var round = 0
    while (round < 2) {
        twice {
            if (count < 100) {
                count = count + 1
            }
        }
        round = round + 1
    }
    twice {
        if (round == 2) {
            last = "set"
            count = count + 2
        }
    }
    if (count != 8) return "count"
    if (last != "set") return "last"
    return "OK"
}
"#,
        "wasmSameModuleInlineOnlyCapturedAssignment",
    );
}

#[test]
fn wasm_declines_an_instruction_wrapper_whose_body_lowering_does_not_model() {
    // PLATFORM-SPECIFIC: pin the Wasm intrinsic-support decline, including its category ownership.
    // `pow` reaches fdlibm's `__ieee754_pow`, whose locals have no initializer: the wrapper is not
    // support code, so its call declines instead of failing compilation.
    expect_wasm_decline(
        r#"
import kotlin.math.pow

fun box(): String = if (2.0.pow(3.0) == 8.0) "OK" else "pow"
"#,
        "wasmUnmodelledInstructionWrapper",
        "a dependency callable whose target code was not emitted",
    );
}

#[test]
fn kotlin_codegen_box_wasm_js_conformance() {
    box_lane::run(&WasmLane {
        target: WasmTarget::Js,
    });
}

#[test]
fn kotlin_codegen_box_wasm_wasi_conformance() {
    box_lane::run(&WasmLane {
        target: WasmTarget::Wasi,
    });
}

fn focused_wasm_lane_available(lane: &WasmLane, stem: &str) -> bool {
    let Some(reason) = lane.unavailable() else {
        return true;
    };
    if std::env::var_os(format!("KRUSTY_REQUIRE_{}_CONFORMANCE", lane.env_infix())).is_some() {
        panic!(
            "{stem}: required {} setup is unavailable: {reason}",
            lane.label()
        );
    }
    false
}

/// A required inline dependency function whose body cannot become a template rejects its caller:
/// it is never compiled as a direct call of the dependency function.
// PLATFORM-SPECIFIC: Wasm expands required KLIB inline bodies in common IR; JVM uses bytecode.
#[test]
fn wasm_a_declined_required_inline_body_rejects_the_caller() {
    let case = super::dependency_template_rejections::DECLINED_REQUIRED_INLINE_BODY;
    super::box_reference_classes::checked_box(case.source, case.stem)
        .expect("pinned kotlinc must run the required-inline rejection fixture");
    let modules = krusty::conformance::split_modules(case.source).expect("valid module test shape");
    let units = krusty::conformance::module_units(&modules);
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        let stem = format!("{}-{}", target.name(), case.stem);
        if !focused_wasm_lane_available(&lane, &stem) {
            return;
        }
        let expected = super::dependency_template_rejections::count_of_from_lib_producer(
            super::dependency_template_rejections::DependencyTarget::wasm(lane.test_target()),
        );
        let outcome =
            lane.compile_modules_and_run(&std::env::temp_dir(), &stem, &units, case.source);
        assert!(
            matches!(&outcome, Outcome::Frontend(message)
                if super::dependency_template_rejections::required_inline_rejection(message, &expected).is_some()),
            "{stem}: {outcome:?}"
        );
    }
}

/// A required stdlib inline function whose template declines rejects its caller.
// PLATFORM-SPECIFIC: this tests the Wasm KLIB template-decline contract, not Kotlin behavior.
#[test]
fn wasm_a_declined_required_stdlib_template_rejects_the_caller() {
    super::dependency_template_rejections::verify_declined_stdlib_source_with_kotlinc();
    let src = super::dependency_template_rejections::DECLINED_STDLIB_INLINE_BODY;
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        let stem = format!("{}-declinedRequiredStdlibTemplate", target.name());
        if !focused_wasm_lane_available(&lane, &stem) {
            return;
        }
        let expected = super::dependency_template_rejections::selected_compare_by(
            super::dependency_template_rejections::DependencyTarget::wasm(lane.test_target()),
        );
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            &stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert!(
            matches!(&outcome, Outcome::Frontend(message)
                if super::dependency_template_rejections::required_inline_rejection(message, &expected).is_some()),
            "{stem}: {outcome:?}"
        );
    }
}

/// Run `src` directly against both Wasm targets' KLIBs and runtimes; every target must answer `OK`.
/// Skips only where a directly invoked lane cannot run; target-owned harness passes require it.
fn expect_wasm_box(src: &str, stem: &str) {
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if !focused_wasm_lane_available(&lane, stem) {
            return;
        }
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert_eq!(outcome, Outcome::Pass, "{stem} on {}", target.name());
    }
}

/// Require both Wasm targets to decline `src` with exactly `reason`.
fn expect_wasm_decline(src: &str, stem: &str, reason: &str) {
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if !focused_wasm_lane_available(&lane, stem) {
            return;
        }
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert_eq!(
            outcome,
            Outcome::Declined(reason.to_string()),
            "{stem} on {}",
            target.name()
        );
    }
}

// PLATFORM-SPECIFIC: exercise the shared box representation at Wasm's dependency call boundary.
#[test]
fn wasm_nested_source_and_dependency_arguments_share_boxing() {
    let src = include_str!("box_regressions/genericDependencyNestedSource.kt");
    super::box_reference_classes::checked_box(src, "numericDependencyNestedSource")
        .expect("the exact Kotlin compiler must run the nested source-call fixture");
    let modules = krusty::conformance::split_modules(src).expect("valid module fixture");
    let units = krusty::conformance::module_units(&modules);
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, "numericDependencyNestedSource"),
            "the owned nested source-call fixture requires its Wasm lane"
        );
        assert_eq!(
            lane.compile_modules_and_run(
                &std::env::temp_dir(),
                "numericDependencyNestedSource",
                &units,
                src,
            ),
            Outcome::Pass,
            "nested source argument on {}",
            target.name(),
        );
    }
}

#[test]
fn wasm_class_properties_export_their_checked_accessors() {
    for (source, stem) in [
        (
            include_str!("box_regressions/classPropertyDependencyExports.kt"),
            "classPropertyDependencyExports",
        ),
        (
            include_str!("box_regressions/delegatedPropertyDependencyExports.kt"),
            "delegatedPropertyDependencyExports",
        ),
    ] {
        super::box_reference_classes::checked_box(source, stem)
            .expect("pinned kotlinc must run the whole property export module graph");
        let modules = krusty::conformance::split_modules(source).expect("valid module fixture");
        let units = krusty::conformance::module_units(&modules);
        for target in WasmTarget::ALL {
            let lane = WasmLane { target };
            assert!(focused_wasm_lane_available(&lane, stem));
            assert_eq!(
                lane.compile_modules_and_run(&std::env::temp_dir(), stem, &units, source),
                Outcome::Pass,
                "{stem} on {}",
                target.name()
            );
        }
    }
}

// PLATFORM-SPECIFIC: Wasm's checked-access default and host trap are independent of the portable
// reference-array value/identity/effect-order case in the shared box inventory.
const REFERENCE_ARRAY_BOUNDS: &str = r#"fun captured(index: Int): Throwable? {
    try {
        val value = arrayOf("value")[index]
        return null
    } catch (failure: Throwable) {
        try { throw failure } catch (same: Throwable) {
            return if (same === failure) same else null
        }
    }
}

fun check(failure: Throwable?): String {
    if (failure == null) return "missing failure or changed identity"
    if (failure !is IndexOutOfBoundsException) return "wrong bounds type"
    if (failure !is RuntimeException) return "wrong parent type"
    if (failure.message != null) return "wrong message"
    return "OK"
}

fun box(): String {
    val negative = captured(-1)
    val high = captured(1)
    if (negative === high) return "reused exception"
    val first = check(negative)
    if (first != "OK") return first
    return check(high)
}
"#;

// PLATFORM-SPECIFIC: an out-of-range store whose value has side effects reaches the bounds check
// only after the receiver, the index and the value ran. With range checks, that check throws
// `IndexOutOfBoundsException`; without them (the 2.4.0 and 2.4.10 default) the store is a host trap.
// A throwing value at an out-of-range index, and the in-range order, are portable and stay in
// `tests/box_regressions/referenceArrayStoreEvaluatesValueBeforeBoundsCheck.kt`.
const REFERENCE_ARRAY_STORE_BOUNDS: &str = r#"class Tag(val text: String)

var trace = ""

fun slots(): Array<Tag> {
    trace += "a"
    return arrayOf(Tag("0"))
}

fun at(index: Int): Int {
    trace += "i"
    return index
}

fun stored(text: String): Tag {
    trace += "v"
    return Tag(text)
}

fun box(): String {
    try {
        slots()[at(1)] = stored("y")
        return "out of range stored"
    } catch (expected: Throwable) {
        if (expected !is IndexOutOfBoundsException) return "out of range threw another exception"
        if (trace != "aiv") return "out of range: $trace"
    }
    return "OK"
}
"#;

// PLATFORM-SPECIFIC: the shared program selects checked Wasm access on every release. The
// reference invocation below proves compile acceptance of the same option, not reference runtime.
#[test]
fn explicit_array_checks_share_metadata_with_the_reference_compiler() {
    use krusty::backend::Backend;
    let source = include_str!("box_regressions/arrayStoreIndexAfterValue.kt");
    let options = krusty::conformance::WasmCompilerOptions::from_source(source)
        .expect("the original case's compilation metadata");
    assert_eq!(options.array_range_checks(), Some(true));
    let (sources, java) = krusty::conformance::split_files(source);
    assert!(java.is_empty());
    let [(stem, written_source)] = sources.as_slice() else {
        panic!("one complete array-store source")
    };
    assert_eq!(
        krusty::conformance::WasmCompilerOptions::from_source(written_source),
        Ok(krusty::conformance::WasmCompilerOptions::default()),
        "the pre-FILE option belongs to compilation context, not the split program"
    );
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(&lane, "explicitArrayChecks"));
        let work = super::common::scratch_dir().expect("array check compilation directory");
        let path = work.join(format!("{stem}.kt"));
        std::fs::write(&path, written_source).expect("write the complete split program");
        let compilation_target = WasmBackend::new(target).compilation_target();
        let reference_arguments = options
            .reference_arguments(compilation_target)
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(reference_arguments, ["-Xwasm-enable-array-range-checks"]);
        let (reference, _) = crate::reference_invocation_support::run_reference(
            compilation_target,
            &work,
            &[path],
            &reference_arguments,
            false,
        )
        .expect("the exact Wasm reference compiler");
        assert_eq!(reference.status, 0, "{}", reference.stderr);
        assert_eq!(
            lane.compile_and_run(&work, "explicitArrayChecks", &sources, source),
            Outcome::Pass,
            "the product must execute the entire checked store-order program"
        );
        std::fs::remove_dir_all(work).expect("remove array check compilation directory");
    }
}

// PLATFORM-SPECIFIC: both the Kotlin exception and the unchecked host trap are observable here.
#[test]
fn wasm_reference_array_bounds_preserve_reference_defaults_and_explicit_options() {
    expect_reference_array_bounds(REFERENCE_ARRAY_BOUNDS, "referenceArrayBounds");
}

// PLATFORM-SPECIFIC: see `REFERENCE_ARRAY_STORE_BOUNDS`.
#[test]
fn wasm_reference_array_store_bounds_follow_the_value_under_every_bounds_policy() {
    expect_reference_box(REFERENCE_ARRAY_STORE_BOUNDS, "referenceArrayStoreBounds");
    expect_reference_array_bounds(REFERENCE_ARRAY_STORE_BOUNDS, "referenceArrayStoreBounds");
}

/// Run `source` under the release's default bounds policy and both explicit ones: a checked access
/// must run `box()` to `OK`, and an unchecked one must stop at the exact host trap.
fn expect_reference_array_bounds(source: &str, name: &str) {
    use krusty::kotlin_version::{KotlinVersion, ReleaseRecords};
    const DEFAULTS: ReleaseRecords<bool> = ReleaseRecords::new(&[
        (KotlinVersion::V2_4_0, false),
        (KotlinVersion::V2_4_20, true),
    ]);
    assert_eq!(DEFAULTS.check(), Ok(()));
    let default = *DEFAULTS
        .get(krusty::kotlin_version::target())
        .expect("supported release");
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if !focused_wasm_lane_available(&lane, name) {
            return;
        }
        for requested in [None, Some(false), Some(true)] {
            let checked = requested.unwrap_or(default);
            let mut backend = WasmBackend::new(target).with_entry(Entry::Box);
            if let Some(requested) = requested {
                backend = backend.with_array_range_checks(requested);
            }
            let stem = format!("{name}{requested:?}");
            let mut program = box_lane::compile(
                &[("Bounds.kt".to_owned(), source.to_owned())],
                source,
                lane.test_target(),
                &backend,
                "box",
                DECLINE_PREFIX,
            )
            .unwrap_or_else(|outcome| panic!("{stem}: {outcome:?}"));
            if !checked {
                // Observe the exact host trap without altering the generated Wasm or its loader.
                // Every unexpected exception and normal completion remains a test failure.
                let loader = program
                    .artifacts
                    .iter_mut()
                    .find(|(name, _)| name == "box.mjs")
                    .expect("generated loader");
                loader.0 = "bounds-program.mjs".to_owned();
                program.artifacts.push(("box.mjs".to_owned(), br#"try {
    await import('./bounds-program.mjs');
    throw new Error('unchecked array access unexpectedly completed');
} catch (failure) {
    if (!(failure instanceof WebAssembly.RuntimeError) || failure.message !== 'array element access out of bounds') throw failure;
    process.stdout.write('OK');
}
"#.to_vec()));
            }
            assert_eq!(
                lane.run_artifacts(&std::env::temp_dir(), &stem, "bounds", program),
                Outcome::Pass,
                "{stem} on {}",
                target.name()
            );
        }
    }
}

/// The shared `unsignedValueBoxes` program: a nullable unsigned value, and one read as `Any`, is a
/// box of its own classifier, never the signed box of the same width.
#[test]
fn wasm_shared_unsigned_value_boxes_program() {
    let src = include_str!("box_regressions/unsignedValueBoxes.kt");
    expect_reference_box(src, "unsignedValueBoxes");
    expect_wasm_box(src, "unsignedValueBoxes");
}

// PLATFORM-SPECIFIC: a reference assertion cannot implement nullable scalar unboxing.
fn expect_reference_box(src: &str, stem: &str) {
    let jdk = super::common::jdk_modules();
    let classpath = super::common::classpath_jars_for(src);
    let reference = super::box_reference_classes::reference_compile(
        src,
        stem,
        &classpath,
        krusty::conformance::BoxJdk::Full { root: Some(&jdk) },
        krusty::conformance::COROUTINE_HELPERS,
    )
    .expect("the exact Kotlin compiler must compile the fixture");
    let classes = reference.flatten().into_iter().collect::<Vec<_>>();
    let entry = super::common::find_box_class(&classes).expect("reference fixture must emit box()");
    assert_eq!(
        super::common::run_box(&classes, &entry, &classpath),
        Some("OK".to_string()),
        "{stem}: the exact Kotlin compiler must run the fixture"
    );
}

// PLATFORM-SPECIFIC: a reference assertion cannot implement nullable scalar unboxing.
#[test]
fn wasm_nullable_scalar_assertions_decline_without_an_unboxing_contract() {
    for (stem, scalar) in [
        ("nullableIntAssertion", "Int"),
        ("nullableLongAssertion", "Long"),
        ("nullableFloatAssertion", "Float"),
        ("nullableDoubleAssertion", "Double"),
    ] {
        let src =
            format!("fun read(value: {scalar}?): {scalar} = value!!\nfun box(): String = \"OK\"");
        expect_reference_box(&src, stem);
        expect_wasm_decline(
            &src,
            stem,
            "a not-null assertion requiring a scalar reference conversion",
        );
    }
}

// PLATFORM-SPECIFIC: a suspend function value is a function value whose implementation takes the
// caller's continuation. Passing one without invoking it answers the same as on the JVM.
#[test]
fn wasm_suspend_function_values_are_function_values() {
    let src = "fun take(f: suspend () -> Int): Int = 1\nfun box(): String = if (take { 2 } == 1) \"OK\" else \"fail\"";
    expect_reference_box(src, "suspendFunctionValue");
    expect_wasm_box(src, "suspendFunctionValue");
}

/// The shared receiver-reuse program, including its non-suspending suspend lambda.
#[test]
fn wasm_apply_receiver_single_store_runs() {
    let src = include_str!("box_regressions/applyReceiverSingleStore.kt");
    expect_reference_box(src, "applyReceiverSingleStore");
    expect_wasm_box(src, "applyReceiverSingleStore");
}

#[test]
fn wasm_parameterless_start_coroutine_delivers_its_completion() {
    let src = include_str!("box_regressions/parameterlessStartCoroutineCompletion.kt");
    expect_reference_box(src, "parameterlessStartCoroutineCompletion");
    expect_wasm_box(src, "parameterlessStartCoroutineCompletion");
}

#[test]
fn wasm_continuation_factory_value_is_not_null() {
    let src = include_str!("box_regressions/continuationFactoryValue.kt");
    expect_reference_box(src, "continuationFactoryValue");
    expect_wasm_box(src, "continuationFactoryValue");
}

#[test]
fn wasm_start_coroutine_evaluates_the_receiver_first() {
    let src = include_str!("box_regressions/startCoroutineOperandOrder.kt");
    expect_reference_box(src, "startCoroutineOperandOrder");
    expect_wasm_box(src, "startCoroutineOperandOrder");
}

#[test]
fn wasm_continuation_hash_code_is_stable() {
    let src = include_str!("box_regressions/continuationIdentityHash.kt");
    expect_reference_box(src, "continuationIdentityHash");
    expect_wasm_box(src, "continuationIdentityHash");
}

#[test]
fn wasm_continuation_is_its_interface_and_keeps_its_context() {
    let src = include_str!("box_regressions/continuationIdentity.kt");
    expect_reference_box(src, "continuationIdentity");
    expect_wasm_box(src, "continuationIdentity");
}

// PLATFORM-SPECIFIC: the reference compiler runs this through the stdlib. Wasm declines it: the
// completion is a different object from the one the factory allocates, and emitting that call
// would fail the cast.
#[test]
fn wasm_foreign_start_coroutine_completion_declines() {
    let src = include_str!("foreignStartCoroutineCompletion.kt");
    expect_reference_box(src, "foreignStartCoroutineCompletion");
    expect_wasm_decline(
        src,
        "foreignStartCoroutineCompletion",
        "a startCoroutine completion that is not the factory continuation",
    );
}

// PLATFORM-SPECIFIC: the reference compiler calls `resumeWith` on a `Continuation` it cannot prove
// is the factory object. Wasm tests the value and leaves a different object untouched, so the
// program finishes without trapping and without calling that resume.
#[test]
fn wasm_continuation_typed_foreign_completion_is_not_called() {
    let src = include_str!("foreignContinuationTypedCompletion.kt");
    expect_wasm_box(src, "foreignContinuationTypedCompletion");
}

#[test]
fn wasm_continuation_from_another_file_is_its_interface() {
    let src = include_str!("box_regressions/continuationAcrossFiles.kt");
    let (blocks, java) = krusty::conformance::split_files(src);
    assert!(java.is_empty());
    let sources: Vec<(&str, &str)> = blocks
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect();
    super::common::expect_box_ok_files_with_stdlib(&sources, "continuationAcrossFiles");
    let files: Vec<(String, String)> = blocks
        .iter()
        .map(|(name, source)| (format!("{name}.kt"), source.clone()))
        .collect();
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        let stem = "continuationAcrossFiles";
        if !focused_wasm_lane_available(&lane, stem) {
            return;
        }
        let outcome = lane.compile_and_run(&std::env::temp_dir(), stem, &files, src);
        assert_eq!(outcome, Outcome::Pass, "{stem} on {}", target.name());
    }
}

// A lambda or callable reference converted to a `fun interface` becomes an object of that
// interface whose member forwards to the function value.
#[test]
fn wasm_sam_converted_function_values_implement_their_interface() {
    for (stem, src) in [
        (
            "samConvertedLambda",
            include_str!("box_regressions/samConvertedLambda.kt"),
        ),
        (
            "samConvertedReference",
            include_str!("box_regressions/samConvertedReference.kt"),
        ),
    ] {
        expect_reference_box(src, stem);
        expect_wasm_box(src, stem);
    }
}

// PLATFORM-SPECIFIC: the scalar boundary guard must preserve existing reference assertions.
#[test]
fn wasm_nullable_reference_assertions_keep_their_existing_carrier() {
    let src = "fun read(value: String?): String = value!!\nfun box(): String = read(\"OK\")";
    expect_reference_box(src, "nullableReferenceAssertion");
    expect_wasm_box(src, "nullableReferenceAssertion");
}

#[test]
fn wasm_string_intrinsics_count_and_read_utf16_code_units() {
    expect_wasm_box(
        r#"
fun box(): String {
    return verify("", "a\u00E9\u4E2D\uD83D\uDE00z")
}

fun verify(empty: String, text: String): String {
    if (empty.length != 0) return "fail: empty length"
    if (text.length != 6) return "fail: length ${text.length}"
    if (text[0] != 'a') return "fail: ascii"
    if (text[1] != '\u00E9') return "fail: latin"
    if (text[2] != '\u4E2D') return "fail: bmp"
    if (text[3] != '\uD83D') return "fail: high surrogate"
    if (text[4] != '\uDE00') return "fail: low surrogate"
    if (text[5] != 'z') return "fail: tail"
    return "OK"
}
"#,
        "wasmStringIntrinsics",
    );
}

#[test]
fn wasm_classes_construct_store_fields_and_dispatch_virtually() {
    expect_wasm_box(
        r#"
open class Shape(val sides: Int) {
    var name: String = "shape"
    constructor(name: String, sides: Int) : this(sides) { this.name = name }
    open fun area(): Int = 0
    fun describe(): Int = area() * 10 + sides
}
class Square(val side: Int) : Shape("square", 4) {
    override fun area(): Int = side * side
}
open class Counter {
    var count = 0
    open fun step(): Int { count += 1; return count }
}
class Doubler : Counter() {
    override fun step(): Int = super.step() * 2
}
fun box(): String {
    val shape: Shape = Square(3)
    if (shape.describe() != 94) return "fail: describe ${shape.describe()}"
    if (shape.name != "square") return "fail: name ${shape.name}"
    val plain = Shape(5)
    if (plain.describe() != 5 || plain.name != "shape") return "fail: plain"
    plain.name = "pentagon"
    if (plain.name != "pentagon") return "fail: write"
    val counter: Counter = Doubler()
    counter.step()
    if (counter.step() != 4 || counter.count != 2) return "fail: super"
    return "OK"
}
"#,
        "wasmClassesDispatch",
    );
}

#[test]
fn wasm_type_tests_and_casts_follow_the_class_hierarchy() {
    expect_wasm_box(
        r#"
interface Named { fun name(): String }
open class Animal
class Cat : Animal(), Named { override fun name() = "cat" }
class Rock : Named { override fun name() = "rock" }
fun nameOf(value: Any?): String {
    if (value is Named) return value.name()
    return "none"
}
fun box(): String {
    val cat: Any = Cat()
    val animal: Animal = Animal()
    if (cat !is Animal || animal is Cat) return "fail: is"
    if (nameOf(cat) != "cat" || nameOf(Rock()) != "rock" || nameOf(animal) != "none") return "fail: interface"
    if (nameOf(null) != "none" || nameOf("text") != "none") return "fail: other"
    val unknown: Any = Cat()
    if ((animal as? Cat) != null || (unknown as? Cat) == null) return "fail: as?"
    val named = cat as Named
    if (named.name() != "cat") return "fail: as"
    val nothing: Any? = null
    if ((nothing as Cat?) != null) return "fail: nullable as"
    return "OK"
}
"#,
        "wasmClassesTypeTests",
    );
}

#[test]
fn wasm_object_declarations_are_built_once_on_first_use() {
    expect_wasm_box(
        r#"
var built = 0
object Registry {
    var size = 0
    init { built += 1 }
    fun add(): Int { size += 1; return size }
}
fun box(): String {
    if (built != 0) return "fail: eager"
    Registry.add()
    if (Registry.add() != 2 || built != 1) return "fail: ${Registry.size} $built"
    return "OK"
}
"#,
        "wasmClassesObjects",
    );
}

#[test]
fn wasm_class_equality_and_rendering_use_the_class_own_members() {
    expect_wasm_box(
        r#"
class Point(val x: Int, val y: Int) {
    override fun equals(other: Any?): Boolean = other is Point && other.x == x && other.y == y
    override fun hashCode(): Int = x * 31 + y
    override fun toString(): String = "Point"
}
class Plain
fun box(): String {
    val a = Point(1, 2)
    if (a != Point(1, 2) || a == Point(2, 1)) return "fail: equals"
    val p = Plain()
    if (p != p || p == Plain()) return "fail: identity"
    if ("$a" != "Point") return "fail: toString"
    return "OK"
}
"#,
        "wasmClassesAnyMembers",
    );
}

#[test]
fn wasm_declines_kotlin_any_rendering_it_has_no_body_for() {
    expect_wasm_decline(
        "class Plain\nfun box(): String = if (\"${Plain()}\" == \"\") \"fail\" else \"OK\"",
        "wasmClassesDefaultToString",
        "`kotlin.Any.toString`'s own rendering",
    );
}

// A generated hash dispatches a property's value through its table's `hashCode` slot. A class
// that keeps `kotlin.Any`'s identity hash has no body there, whether the property names that class
// or erases it to `Any`, which can hold an instance of every class in the file. Each `Holder`
// renders itself, since its generated `toString` would reach `Any`'s rendering first.
#[test]
fn wasm_declines_a_generated_hash_that_can_reach_the_identity_hash() {
    let reason = "`kotlin.Any.hashCode`'s identity hash";
    expect_wasm_decline(
        "class Plain\ndata class Holder(val value: Plain) { override fun toString() = \"Holder\" }\n\
         fun box(): String = if (Holder(Plain()).hashCode() == 0) \"zero\" else \"OK\"",
        "wasmGeneratedHashOfAClassProperty",
        reason,
    );
    expect_wasm_decline(
        "class Plain\ndata class Holder(val value: Any) { override fun toString() = \"Holder\" }\n\
         fun box(): String = if (Holder(Plain()).hashCode() == 0) \"zero\" else \"OK\"",
        "wasmGeneratedHashOfAnErasedProperty",
        reason,
    );
    // Runtime exception objects come from the exception runtime rather than this file's classes,
    // and an erased property can hold one that the file constructs or catches.
    expect_wasm_decline(
        "data class Holder(val value: Any) { override fun toString() = \"Holder\" }\n\
         fun box(): String = if (Holder(IllegalStateException()).hashCode() == 0) \"zero\" else \"OK\"",
        "wasmGeneratedHashOfAConstructedRuntimeException",
        reason,
    );
    expect_wasm_decline(
        "data class Holder(val value: Any) { override fun toString() = \"Holder\" }\n\
         fun caught(text: String?): Any = try { text!! } catch (e: NullPointerException) { e }\n\
         fun box(): String = if (Holder(caught(null)).hashCode() == 0) \"zero\" else \"OK\"",
        "wasmGeneratedHashOfACaughtRuntimeException",
        reason,
    );
}

#[test]
fn wasm_generated_members_keep_the_whole_portable_scalar_and_reference_control() {
    expect_wasm_box(
        include_str!("box_regressions/dataClassEqualsAndHashCode.kt"),
        "wasmDataClassEqualsAndHashCode",
    );
}

#[test]
fn wasm_generated_members_preserve_closure_identity() {
    expect_wasm_box(
        include_str!("box_regressions/dataClassClosureIdentity.kt"),
        "wasmDataClassClosureIdentity",
    );
}

#[test]
fn wasm_generated_members_keep_erased_array_hashes_stable_through_mutation() {
    expect_wasm_box(
        include_str!("box_regressions/dataClassErasedArrayIdentityHash.kt"),
        "wasmDataClassErasedArrayIdentityHash",
    );
}

#[test]
fn wasm_generated_members_compare_callable_declarations_and_receivers() {
    for (source, stem) in [
        (
            include_str!("box_regressions/dataClassErasedClosureIdentity.kt"),
            "wasmDataClassErasedClosureIdentity",
        ),
        (
            include_str!("box_regressions/dataClassCallableReferenceEquality.kt"),
            "wasmDataClassCallableReferenceEquality",
        ),
        (
            include_str!("box_regressions/dataClassCallableReferenceSelfEquality.kt"),
            "wasmDataClassCallableReferenceSelfEquality",
        ),
        (
            include_str!("box_regressions/dataClassOverloadedConstructorReferences.kt"),
            "wasmDataClassOverloadedConstructorReferences",
        ),
        (
            include_str!("box_regressions/constructorReferenceDeclaredSignature.kt"),
            "wasmConstructorReferenceDeclaredSignature",
        ),
        (
            include_str!("box_regressions/dataClassNullableBoundReference.kt"),
            "wasmDataClassNullableBoundReference",
        ),
    ] {
        expect_wasm_box(source, stem);
    }
}

#[test]
fn wasm_callable_references_share_identity_across_independent_module_analyses() {
    for (source, stem) in [
        (
            include_str!("box_regressions/dataClassReferenceAcrossModules.kt"),
            "wasmDataClassReferenceAcrossModules",
        ),
        (
            include_str!("box_regressions/constructorReferenceAcrossModules.kt"),
            "wasmConstructorReferenceAcrossModules",
        ),
    ] {
        super::box_reference_classes::checked_box(source, stem)
            .expect("the exact Kotlin compiler must run the whole reference-identity fixture");
        let modules = krusty::conformance::split_modules(source).expect("valid module fixture");
        let units = krusty::conformance::module_units(&modules);
        for target in WasmTarget::ALL {
            let lane = WasmLane { target };
            assert!(focused_wasm_lane_available(&lane, stem));
            assert_eq!(
                lane.compile_modules_and_run(&std::env::temp_dir(), stem, &units, source),
                Outcome::Pass,
                "{stem} on {}",
                target.name(),
            );
        }
    }
}

#[test]
fn wasm_a_generated_hash_of_an_erased_property_dispatches_when_every_class_has_a_body() {
    expect_wasm_box(
        include_str!("box_regressions/dataClassErasedPropertyHash.kt"),
        "wasmGeneratedHashOfAnErasedPropertyWithBodies",
    );
}

/// Enum classes realize through the shared enum pass, so the program the Wasm lanes used to
/// decline by name now runs.
#[test]
fn wasm_runs_an_enum_class() {
    expect_wasm_box(
        "enum class Color { RED }\nfun box(): String = \"OK\"",
        "wasmClassesEnum",
    );
}

#[path = "wasm_value_hash.rs"]
mod value_hash;

#[path = "wasm_dependency_constructor_exports.rs"]
mod dependency_constructor_exports;

#[path = "wasm_escaping_closures.rs"]
mod escaping_closures;

#[path = "wasm_type_of.rs"]
mod type_of;

#[test]
fn wasm_finally_preserves_values_original_exceptions_and_cleanup_precedence() {
    expect_wasm_box(
        include_str!("box_regressions/finallyValueAndKotlinThrow.kt"),
        "wasmFinallyValueAndKotlinThrow",
    );
}

#[test]
fn wasm_data_class_unit_property_uses_units_fixed_hash() {
    expect_wasm_box(
        include_str!("box_regressions/knownPrimaryConstructorRoles.kt"),
        "wasmKnownPrimaryConstructorRoles",
    );
}

#[path = "wasm_unsigned_decimal.rs"]
mod unsigned_decimal;
