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
use krusty::wasm::{WasmBackend, WasmTarget};

use super::box_lane::{self, Lane, Libraries, Outcome};
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

    /// The exact target stdlib KLIB: wasm-js and wasm-wasi publish different platform surfaces.
    fn libraries(&self) -> Libraries {
        Libraries::StdlibKlib
    }

    fn unavailable(&self) -> Option<String> {
        if !self.libraries().available(self.test_target()) {
            return Some("no stdlib and JDK modules to analyze against".to_string());
        }
        node_unavailable()
    }

    fn not_applicable(&self, src: &str) -> Option<&'static str> {
        if !krusty::conformance::backend_targeted(src, &self.tokens()) {
            return Some("targeted at another backend");
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
        let backend = WasmBackend::new(self.target).with_entry(Entry::Box);
        let program = match box_lane::compile(
            sources,
            directive_source,
            self.libraries(),
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
        let backend = WasmBackend::new(self.target).with_entry(Entry::Box);
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
            scratch,
            stem,
            modules,
            directive_source,
            self.libraries(),
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
    assert_eq!(js.libraries(), Libraries::StdlibKlib);
    assert_eq!(wasi.libraries(), Libraries::StdlibKlib);
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
        Some("targeted at another backend")
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
        let outcome =
            lane.compile_modules_and_run(&std::env::temp_dir(), &stem, &units, case.source);
        assert!(
            matches!(&outcome, Outcome::Frontend(message)
                if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
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
        let outcome = lane.compile_and_run(
            &std::env::temp_dir(),
            &stem,
            &[(format!("{stem}.kt"), src.to_string())],
            src,
        );
        assert!(
            matches!(&outcome, Outcome::Frontend(message)
                if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
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

// PLATFORM-SPECIFIC: dependency ABI adapters must not change nested source-call conversions.
#[test]
fn wasm_numeric_dependency_adapters_do_not_enter_source_argument_calls() {
    let src = r#"
// MODULE: lib
fun <T> dependencyKeep(value: T): T = value
// MODULE: main(lib)
fun sourceAny(value: Any): Any = value
fun box(): String { dependencyKeep(sourceAny(42)); return "OK" }
"#;
    super::box_reference_classes::checked_box(src, "numericDependencyNestedSource")
        .expect("the exact Kotlin compiler must run the nested source-call fixture");
    let modules = krusty::conformance::split_modules(src).expect("valid module fixture");
    let units = krusty::conformance::module_units(&modules);
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        if !focused_wasm_lane_available(&lane, "numericDependencyNestedSource") {
            return;
        }
        assert_eq!(
            lane.compile_modules_and_run(
                &std::env::temp_dir(),
                "numericDependencyNestedSource",
                &units,
                src,
            ),
            Outcome::Declined("a coercion to `kotlin.Any`".to_owned()),
            "nested source argument on {}",
            target.name(),
        );
    }
}

// PLATFORM-SPECIFIC: a reference assertion cannot implement nullable scalar unboxing.
fn expect_reference_assertion_box(src: &str, stem: &str) {
    let jdk = super::common::jdk_modules();
    let classpath = super::common::classpath_jars_for(src);
    let reference = super::box_reference_classes::reference_compile(
        src,
        stem,
        &classpath,
        krusty::conformance::BoxJdk::Full { root: Some(&jdk) },
        krusty::conformance::COROUTINE_HELPERS,
    )
    .expect("the exact Kotlin compiler must compile the nullable assertion fixture");
    let classes = reference.flatten().into_iter().collect::<Vec<_>>();
    let entry = super::common::find_box_class(&classes).expect("reference fixture must emit box()");
    assert_eq!(
        super::common::run_box(&classes, &entry, &classpath),
        Some("OK".to_string()),
        "{stem}: the exact Kotlin compiler must run the nullable assertion fixture"
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
        expect_reference_assertion_box(&src, stem);
        expect_wasm_decline(
            &src,
            stem,
            "a not-null assertion requiring a scalar reference conversion",
        );
    }
}

// PLATFORM-SPECIFIC: the scalar boundary guard must preserve existing reference assertions.
#[test]
fn wasm_nullable_reference_assertions_keep_their_existing_carrier() {
    let src = "fun read(value: String?): String = value!!\nfun box(): String = read(\"OK\")";
    expect_reference_assertion_box(src, "nullableReferenceAssertion");
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

#[test]
fn wasm_declines_an_enum_class_by_name() {
    expect_wasm_decline(
        "enum class Color { RED }\nfun box(): String = \"OK\"",
        "wasmClassesEnum",
        "an enum class (`Color`)",
    );
}
