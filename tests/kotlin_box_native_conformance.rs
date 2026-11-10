//! The `codegen/box` corpus through krusty's native pipeline.
//!
//! Every case is a Kotlin source set with `fun box(): String` that returns `"OK"` when the compiler
//! got it right. Its Kotlin files are compiled together by krusty's own code generator with a
//! program entry that prints `box()`'s result, linked by krusty's linker against the prebuilt
//! runtime, and RUN; the executable must print `OK` and exit cleanly. The driver, the verdict and
//! the committed ratchet are every runnable target's (`box_lane`); this file is what is Native's.
//!
//! Multi-file cases use the production source-set path. Multi-module cases are analyzed one source
//! module at a time against generated KLIB dependencies, while one target emission session retains
//! the program backend state; folded `dependsOn` sources remain common inputs rather than being
//! flattened into platform sources.

use std::path::Path;
use std::process::Command;

use krusty::conformance::TestTarget;
use krusty_native::{CraneliftBackend, Entry, NativeTarget};

use super::box_lane::{self, Lane, ModuleCase, Outcome};
use super::box_ratchet::Platform;

/// The prefix the native backend puts on every decline; what follows names the construct.
const DECLINE_PREFIX: &str = "krusty: the native backend does not support ";

fn host() -> Option<NativeTarget> {
    let target = NativeTarget::host()?;
    (krusty_native::can_link(target) && box_lane::stdlib_klib_available(TestTarget::Native))
        .then_some(target)
}

/// Why a case is outside this harness before any compilation, or `None` when it applies.
fn not_applicable(src: &str) -> Option<&'static str> {
    // A case that NAMES its backends and does not name this one is written for a platform this is
    // not: `// TARGET_BACKEND: JVM` over `object : Runnable` is a program about the JDK, and
    // compiling it here means resolving `java.lang.Runnable` for a target that has no JDK. The
    // widening that admitted them was deliberate and its own comment said when to withdraw it —
    // "when the native target grows its own frontend semantics (klib ingestion, phase 7)" — which
    // is what the klib provider is.
    if !krusty::conformance::backend_targeted(src, &["NATIVE"]) {
        return Some(box_lane::TARGETED_AT_ANOTHER_BACKEND);
    }
    // Backend mutes are target-specific. A JVM mute cannot hide a shared-frontend or common-IR
    // failure from the Native lane; that failure belongs in this lane's exact outcome ratchet.
    if krusty::conformance::backend_muted(src, &["NATIVE"]) {
        return Some("muted on the native backend");
    }
    if krusty::conformance::directive(src, "FULL_JDK") {
        return Some("requires the JDK runtime");
    }
    None
}

#[test]
fn native_applicability_uses_native_directives_not_jvm_mutes() {
    assert_eq!(
        not_applicable("// TARGET_BACKEND: NATIVE\n// IGNORE_BACKEND: JVM\nfun box() = \"OK\""),
        None,
        "a JVM-only mute must not remove Native coverage"
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: NATIVE\n// IGNORE_BACKEND: NATIVE"),
        Some("muted on the native backend")
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: JVM\nfun box() = \"OK\""),
        Some(box_lane::TARGETED_AT_ANOTHER_BACKEND)
    );
    assert_eq!(
        not_applicable("// TARGET_BACKEND: ANY\nfun box() = \"OK\""),
        None
    );
    assert_eq!(
        not_applicable("// IGNORE_BACKEND: ANY\nfun box() = \"OK\""),
        Some("muted on the native backend")
    );
    assert_eq!(
        not_applicable("// KJS_WITH_FULL_RUNTIME\n// JVM_TARGET: 1.8\nfun box() = \"OK\""),
        None,
        "another backend's runner options do not remove Native coverage"
    );
    assert_eq!(
        not_applicable("import kotlin.experimental.ExperimentalTypeInference\nfun box() = \"OK\""),
        None,
        "an annotation name is not an unmodeled compiler option"
    );
    assert_eq!(
        not_applicable("// MODULE: lib\nfun answer() = \"OK\""),
        None,
        "a missing harness topology is an expected failure, not an exclusion"
    );
}

/// Why an applicable case cannot yet be constructed by this runner.
fn harness_limitation(src: &str) -> Option<&'static str> {
    // JVM/JS runner switches (`KJS_WITH_FULL_RUNTIME`, `JVM_DEFAULT_MODE`, `LAMBDAS`, …) do not
    // configure a Native compilation. These two options do change this target's semantics, but the
    // missing option model is our failure and therefore stays in the denominator.
    if src
        .lines()
        .any(|line| line.starts_with("// FREE_COMPILER_ARGS:") && line.contains("genericSafeCasts"))
        || krusty::conformance::directive(src, "PROPERTY_LAZY_INITIALIZATION")
    {
        return Some("Native compiler options are not modeled by the harness");
    }
    None
}

#[test]
fn applicable_harness_gaps_are_expected_failures() {
    assert_eq!(
        harness_limitation("// FREE_COMPILER_ARGS: -XgenericSafeCasts\nfun box() = \"OK\""),
        Some("Native compiler options are not modeled by the harness")
    );
}

struct NativeLane {
    target: Option<NativeTarget>,
}

impl NativeLane {
    fn run_artifacts(
        &self,
        scratch: &Path,
        stem: &str,
        suffix: &str,
        program: box_lane::CompiledProgram,
    ) -> Outcome {
        let target = self.target.expect("checked by `unavailable`");
        let objects = program
            .artifacts
            .into_iter()
            .filter_map(|(name, object)| name.ends_with(".o").then_some(object))
            .collect::<Vec<_>>();
        if objects.is_empty() {
            return Outcome::Frontend(
                "the frontend produced no checked file and said nothing".to_string(),
            );
        }
        let object_refs = objects.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let image = match krusty_native::link_program_with_libraries(
            &object_refs,
            &program.native_imports,
            target,
        ) {
            Ok(image) => image,
            Err(error) => return Outcome::Failed(format!("link: {error}")),
        };
        let executable = scratch.join(format!(
            "{stem}-{suffix}-{}-{}",
            std::process::id(),
            rayon::current_thread_index().unwrap_or(0)
        ));
        if let Err(error) = std::fs::write(&executable, &image) {
            return Outcome::Harness(format!("writing the executable: {error}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(error) =
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            {
                return Outcome::Harness(format!("setting executable permissions: {error}"));
            }
        }
        let outcome = box_lane::run_program(Command::new(&executable).env_clear());
        let _ = std::fs::remove_file(&executable);
        outcome
    }
}

impl Lane for NativeLane {
    fn platform(&self) -> Platform {
        Platform::Native
    }

    fn label(&self) -> &'static str {
        "Native"
    }
    fn env_infix(&self) -> &'static str {
        "NATIVE"
    }

    fn test_target(&self) -> TestTarget {
        TestTarget::Native
    }

    fn unavailable(&self) -> Option<String> {
        self.target.is_none().then(|| {
            "no prebuilt native runtime for this host, or no Kotlin/Native stdlib KLIB".to_string()
        })
    }

    fn not_applicable(&self, src: &str) -> Option<&'static str> {
        not_applicable(src)
    }

    fn harness_limitation(&self, src: &str) -> Option<&'static str> {
        harness_limitation(src)
    }

    fn compile_and_run(
        &self,
        scratch: &Path,
        stem: &str,
        sources: &[(String, String)],
        directive_source: &str,
    ) -> Outcome {
        let target = self.target.expect("checked by `unavailable`");
        let backend = CraneliftBackend::new(target)
            .with_entry(Entry::Box)
            .verified();
        let program = match box_lane::compile(
            sources,
            directive_source,
            TestTarget::Native,
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
        let target = self.target.expect("checked by `unavailable`");
        let backend = CraneliftBackend::new(target)
            .with_entry(Entry::Box)
            .with_entry_module(entry_module.name.clone())
            .verified();
        let features = krusty::features::LangFeatures::from_source(directive_source);
        let metadata = match krusty::klib::backend::KlibBackend::new(
            krusty::klib::write::KlibPlatform::Native,
            krusty::klib::write::KlibStamp::new(
                krusty::kotlin_version::target(),
                features.language_version(),
            ),
        )
        .with_annotations_in_metadata(features.has("AnnotationsInMetadata"))
        .with_native_target(target)
        {
            Ok(metadata) => metadata,
            Err(error) => return Outcome::Harness(error.to_string()),
        };
        let program = match box_lane::compile_modules(
            ModuleCase {
                scratch,
                stem,
                modules,
            },
            directive_source,
            TestTarget::Native,
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
fn kotlin_codegen_box_native_conformance() {
    box_lane::run(&NativeLane { target: host() });
}

fn run_native_abi_case(source: &str, stem: &str) -> Outcome {
    if let Some(reason) = not_applicable(source) {
        return Outcome::NotApplicable(reason);
    }
    let Some(target) = host() else {
        return Outcome::Harness("required Native target/KLIB setup is unavailable".into());
    };
    let Some(answer) =
        super::kotlin_native_multimodule_support::kotlin_native_multimodule_box(source)
    else {
        return Outcome::Harness(
            "required pinned kotlinc-native module oracle is unavailable".into(),
        );
    };
    assert_eq!(
        answer, "OK",
        "{}: pinned kotlinc-native must run the exact KLIB module graph as OK",
        stem
    );
    let modules = krusty::conformance::split_modules(source).expect("valid module test shape");
    let units = krusty::conformance::module_units(&modules);
    let lane = NativeLane {
        target: Some(target),
    };
    lane.compile_modules_and_run(
        &std::env::temp_dir(),
        &format!("native-{stem}"),
        &units,
        source,
    )
}

// PLATFORM-SPECIFIC: This checks the Native oracle's generated module runner. The whole portable
// program is also selected by every backend from the shared box inventory.
#[test]
fn native_reference_runner_calls_the_packaged_final_module_box() {
    let source = include_str!("box_regressions/multimodule/packagedFinalModuleBox.kt");
    let stem = "packagedFinalModuleBox";
    super::box_reference_classes::checked_box(source, stem)
        .expect("pinned kotlinc must run the whole packaged module graph");
    assert_eq!(
        run_native_abi_case(source, stem),
        Outcome::Pass,
        "the Native oracle and product must both call the final module's declared box"
    );
}

#[test]
fn native_class_properties_export_their_checked_accessors() {
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
        assert_eq!(
            run_native_abi_case(source, stem),
            Outcome::Pass,
            "property accessor declarations must retain their module export identities"
        );
    }
}

/// A required inline dependency function whose body cannot become a template rejects its caller:
/// it is never compiled as a direct call of the dependency function.
// PLATFORM-SPECIFIC: Native expands required KLIB inline bodies in common IR; JVM uses bytecode.
#[test]
fn native_a_declined_required_inline_body_rejects_the_caller() {
    let case = super::dependency_template_rejections::DECLINED_REQUIRED_INLINE_BODY;
    super::box_reference_classes::checked_box(case.source, case.stem)
        .expect("pinned kotlinc must run the required-inline rejection fixture");
    let Some(target) = host() else {
        if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some() {
            panic!(
                "{}: required Native target/KLIB setup is unavailable",
                case.stem
            );
        }
        return;
    };
    let modules = krusty::conformance::split_modules(case.source).expect("valid module test shape");
    let units = krusty::conformance::module_units(&modules);
    let lane = NativeLane {
        target: Some(target),
    };
    let outcome =
        lane.compile_modules_and_run(&std::env::temp_dir(), case.stem, &units, case.source);
    assert!(
        matches!(&outcome, Outcome::Frontend(message)
            if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
        "{}: {outcome:?}",
        case.stem
    );
}

/// A required stdlib inline function whose template declines rejects its caller.
// PLATFORM-SPECIFIC: this tests the Native KLIB template-decline contract, not Kotlin behavior.
#[test]
fn native_a_declined_required_stdlib_template_rejects_the_caller() {
    super::dependency_template_rejections::verify_declined_stdlib_source_with_kotlinc();
    let src = super::dependency_template_rejections::DECLINED_STDLIB_INLINE_BODY;
    let stem = "declinedRequiredStdlibTemplate";
    let Some(target) = host() else {
        if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some() {
            panic!("{stem}: required Native target/KLIB setup is unavailable");
        }
        return;
    };
    let lane = NativeLane {
        target: Some(target),
    };
    let outcome = lane.compile_and_run(
        &std::env::temp_dir(),
        stem,
        &[(format!("{stem}.kt"), src.to_string())],
        src,
    );
    assert!(
        matches!(&outcome, Outcome::Frontend(message)
            if super::dependency_template_rejections::required_inline_rejection(message).is_some()),
        "{stem}: {outcome:?}"
    );
}

#[test]
fn native_module_abi_symbols_keep_same_named_internal_classifiers_distinct() {
    // PLATFORM-SPECIFIC: Native value-class syntax and module-qualified runtime/box/unbox symbols.
    let source = "// TARGET_BACKEND: NATIVE\n\
                 // MODULE: left\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
                 // MODULE: right\npackage shared\ninternal class Token(val value: String)\ninternal value class Packed(val value: String)\n\
                 // MODULE: main(left, right)\nfun box() = \"OK\"\n";
    assert_eq!(
        run_native_abi_case(source, "multimoduleNativeAbiSymbols"),
        Outcome::Pass,
        "Native ABI identities must remain distinct across the exact module graph"
    );
}

// PLATFORM-SPECIFIC: Native object markers join source declarations to their KLIB export records.
// Both pinned reference compilers run the same module graph before Native linkage is checked.
#[test]
fn native_callable_reference_identity_survives_a_source_module_export() {
    let source = r#"// MODULE: library
// FILE: Library.kt
package referenceidentity
fun step(value: Int): Int = value + 1
fun different(value: Int): Int = value + 2
fun fromLibrary(): Any = ::step

// MODULE: main(library)
// FILE: Box.kt
import referenceidentity.*
fun box(): String {
    val original = fromLibrary()
    val imported: Any = ::step
    if (original != imported || imported != original) return "declaration identity"
    if (original.hashCode() != imported.hashCode()) return "declaration hash"
    if (original == ::different || ::different == imported) return "different declaration"
    return "OK"
}
"#;
    let stem = "sourceModuleReferenceIdentity";
    super::box_reference_classes::checked_box(source, stem)
        .expect("pinned kotlinc must run the exact reference-identity module graph");
    assert_eq!(
        run_native_abi_case(source, stem),
        Outcome::Pass,
        "Native reference identity must use the source declaration across its KLIB export"
    );
}

#[test]
fn native_generic_class_members_instantiate_their_declared_dependency_exports() {
    let source = include_str!("box_regressions/multimodule/multimoduleGenericClassMembers.kt");
    let stem = "multimoduleGenericClassMembers";
    super::box_reference_classes::checked_box(source, stem)
        .expect("the JVM oracle must run the complete generic-member module graph");
    assert_eq!(
        run_native_abi_case(source, stem),
        Outcome::Pass,
        "class and method formals must instantiate independently without changing the export ABI"
    );
}

/// Run a portable one-file program on the KLIB Native lane, after the pinned JVM compiler ran it
/// to `OK`, which is the answer the Native program must give.
fn run_native_portable_box(source: &str, stem: &str) -> Outcome {
    super::box_reference_classes::checked_box(source, stem)
        .expect("pinned kotlinc must run the portable program as OK");
    let Some(target) = host() else {
        return Outcome::Harness("required Native target/KLIB setup is unavailable".into());
    };
    let lane = NativeLane {
        target: Some(target),
    };
    lane.compile_and_run(
        &std::env::temp_dir(),
        stem,
        &[(format!("{stem}.kt"), source.to_string())],
        source,
    )
}

/// A `StringBuilder` appended to through `Appendable` from a file declaring no `Appendable` of
/// its own: the call goes through the interface member, and the builder is one the runtime makes
/// rather than a class of the program. The stdlib's `joinTo` appends to its buffer this way.
#[test]
fn native_appendable_calls_reach_the_runtime_string_builder() {
    let source = r#"fun put(a: Appendable): Appendable = a.append('y').append("zz")

fun <A : Appendable> bounded(buffer: A): A {
    buffer.append(null)
    return buffer
}

fun box(): String {
    val sb = StringBuilder("x")
    if (put(sb) !== sb) return "fail answer"
    if (bounded(sb).toString() != "xyzznull") return "fail text: $sb"
    return "OK"
}
"#;
    assert_eq!(
        run_native_portable_box(source, "appendableRuntimeBuilder"),
        Outcome::Pass,
        "an `Appendable` call must reach the runtime builder's own `append`"
    );
}

/// A `StringBuilder` and a class of the program both appended to through one `Appendable` call
/// site: each keeps its own `append`.
#[test]
fn native_appendable_calls_reach_the_runtime_string_builder_beside_an_implementor() {
    let source = r#"class Counter : Appendable {
    var count = 0
    override fun append(value: Char): Appendable { count += 1; return this }
    override fun append(value: CharSequence?): Appendable { count += 10; return this }
    override fun append(value: CharSequence?, startIndex: Int, endIndex: Int): Appendable {
        count += 100
        return this
    }
}

fun put(a: Appendable) {
    a.append('y')
    a.append("zz")
}

fun box(): String {
    val sb = StringBuilder()
    put(sb)
    if (sb.toString() != "yzz") return "fail builder: $sb"
    val counter = Counter()
    put(counter)
    if (counter.count != 11) return "fail counter: ${counter.count}"
    return "OK"
}
"#;
    assert_eq!(
        run_native_portable_box(source, "appendableRuntimeBuilderBesideImplementor"),
        Outcome::Pass,
        "the runtime builder and the file's implementor must each keep their own `append`"
    );
}

/// `Appendable.append(value, startIndex, endIndex)` on the runtime's builder, from a file with no
/// implementor and from one whose own implementor shares the call site. A null value appends the
/// range of `"null"`.
#[test]
fn native_appendable_range_appends_reach_the_runtime_string_builder() {
    let alone = r#"class Letters : CharSequence {
    override val length: Int get() = 4
    override fun get(index: Int): Char = 'p' + index
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence =
        toString().subSequence(startIndex, endIndex)
    override fun toString(): String = "pqrs"
}

fun put(a: Appendable): Appendable =
    a.append("abcd", 1, 3).append(null, 1, 3).append(StringBuilder("xyz"), 0, 2)

fun box(): String {
    val sb = StringBuilder(">")
    if (put(sb) !== sb) return "fail answer"
    if (sb.toString() != ">bculxy") return "fail text: $sb"
    (sb as Appendable).append(sb, 0, 2)
    if (sb.toString() != ">bculxy>b") return "fail self: $sb"
    (sb as Appendable).append(Letters(), 1, 3)
    if (sb.toString() != ">bculxy>bqr") return "fail sequence: $sb"
    (sb as Appendable).append("😀", 0, 1)
    (sb as Appendable).append("😀", 1, 2)
    if (sb.toString() != ">bculxy>bqr😀") return "fail pair: $sb"
    return "OK"
}
"#;
    assert_eq!(
        run_native_portable_box(alone, "appendableRangeRuntimeBuilder"),
        Outcome::Pass,
        "a range `append` through `Appendable` must reach the runtime builder"
    );
    let beside = r#"class Counter : Appendable {
    var count = 0
    override fun append(value: Char): Appendable { count += 1; return this }
    override fun append(value: CharSequence?): Appendable { count += 10; return this }
    override fun append(value: CharSequence?, startIndex: Int, endIndex: Int): Appendable {
        count += 100 * (endIndex - startIndex)
        return this
    }
}

fun put(a: Appendable) {
    a.append('y')
    a.append("zz")
    a.append("abc", 0, 1)
    a.append(null, 2, 4)
}

fun box(): String {
    val sb = StringBuilder()
    put(sb)
    if (sb.toString() != "yzzall") return "fail builder: $sb"
    val counter = Counter()
    put(counter)
    if (counter.count != 311) return "fail counter: ${counter.count}"
    return "OK"
}
"#;
    assert_eq!(
        run_native_portable_box(beside, "appendableRangeRuntimeBuilderBesideImplementor"),
        Outcome::Pass,
        "a range `append` must reach the runtime builder and the file's implementor alike"
    );
}

// PLATFORM-SPECIFIC: the bounds a range `append` checks and the exceptions it raises are
// Kotlin/Native's builder's, which the JVM's `java.lang.StringBuilder` words and types otherwise.
// Pinned kotlinc-native runs the same module graph before the product does; the library appends
// through `Appendable` to the builder the final module hands it.
#[test]
fn native_appendable_range_append_checks_its_bounds_as_kotlin_native_does() {
    let source = r#"// TARGET_BACKEND: NATIVE
// MODULE: library
// FILE: Put.kt
package put

fun put(a: Appendable, value: CharSequence?, start: Int, end: Int) {
    a.append(value, start, end)
}

// MODULE: main(library)
// FILE: Box.kt
import put.put

fun failure(value: CharSequence?, start: Int, end: Int): String {
    val sb = StringBuilder("x")
    try {
        put(sb, value, start, end)
    } catch (e: IndexOutOfBoundsException) {
        return "bounds ${e.message} $sb"
    } catch (e: IllegalArgumentException) {
        return "argument ${e.message} $sb"
    }
    return "none $sb"
}

fun box(): String {
    val past = failure("abc", 1, 4)
    if (past != "bounds startIndex: 1, endIndex: 4, size: 3 x") return past
    val negative = failure("abc", -1, 2)
    if (negative != "bounds startIndex: -1, endIndex: 2, size: 3 x") return negative
    val reversed = failure("abc", 2, 1)
    if (reversed != "argument startIndex: 2 > endIndex: 1 x") return reversed
    val nothing = failure(null, 0, 5)
    if (nothing != "bounds startIndex: 0, endIndex: 5, size: 4 x") return nothing
    return "OK"
}
"#;
    assert_eq!(
        run_native_abi_case(source, "appendableRangeBounds"),
        Outcome::Pass,
        "a range `append` on the runtime builder must check its bounds as Kotlin/Native's does"
    );
}

// PLATFORM-SPECIFIC: source getter exports retain the defining module's declared storage ABI.
#[test]
fn native_source_getter_exports_read_declared_generic_storage() {
    // Direct imported construction remains covered by the original graph retained in #2780.
    // This separate getter graph uses the producer's factories and checks every storage form.
    let source = include_str!("box_regressions/multimodule/sourceGetterExports.kt").to_owned();
    let stem = "nativeSourceGetterExports";
    super::box_reference_classes::checked_box(&source, stem)
        .expect("the JVM oracle must run the complete source-getter graph");
    assert_eq!(run_native_abi_case(&source, stem), Outcome::Pass);
}

// PLATFORM-SPECIFIC: a source-module accessor export and its declared Native receiver ABI.
#[test]
fn native_explicit_getter_exports_keep_their_declared_receiver() {
    let source = include_str!("box_regressions/multimodule/explicitGetterExports.kt");
    let stem = "nativeExplicitGetterExports";
    super::box_reference_classes::checked_box(source, stem)
        .expect("the JVM oracle must run the complete explicit-getter graph");
    assert_eq!(run_native_abi_case(source, stem), Outcome::Pass);
}

// PLATFORM-SPECIFIC: a source getter's public ABI differs from its Native backing-field storage.
#[test]
fn native_getter_exports_preserve_distinct_backing_field_storage() {
    let source = include_str!("box_regressions/multimodule/distinctBackingFieldExport.kt");
    let stem = "nativeDistinctBackingFieldExport";
    super::box_reference_classes::checked_box(source, stem)
        .expect("the JVM oracle must run the complete backing-field graph");
    assert_eq!(run_native_abi_case(source, stem), Outcome::Pass);
}

// PLATFORM-SPECIFIC: explicit accessor entries retain boxed class formals and a void setter ABI.
#[test]
fn native_explicit_setter_exports_keep_their_declared_value_parameter() {
    let source = include_str!("box_regressions/multimodule/explicitSetterExports.kt");
    let stem = "nativeExplicitSetterExports";
    super::box_reference_classes::checked_box(source, stem)
        .expect("the JVM oracle must run the complete explicit-setter graph");
    assert_eq!(run_native_abi_case(source, stem), Outcome::Pass);
}

// PLATFORM-SPECIFIC: an external virtual getter must execute its override or decline cleanly.
#[test]
fn native_virtual_field_getter_exports_never_select_base_storage_directly() {
    assert_virtual_getter_export(
        "open class Base { open val value: String = \"base\" }\n\
         class Derived : Base() { override val value: String = \"OK\" }",
        "nativeVirtualFieldGetterExport",
    );
}

// PLATFORM-SPECIFIC: explicit virtual accessor bodies have the same dispatch obligation.
#[test]
fn native_virtual_explicit_getter_exports_never_select_a_base_body_directly() {
    assert_virtual_getter_export(
        "open class Base { open val value: String get() = \"base\" }\n\
         class Derived : Base() { override val value: String get() = \"OK\" }",
        "nativeVirtualExplicitGetterExport",
    );
}

fn assert_virtual_getter_export(declarations: &str, stem: &str) {
    let source = format!(
        "// MODULE: lib\npackage fixture\n{declarations}\n\
         fun produce(): Base = Derived()\n\
         // MODULE: main(lib)\nfun box(): String = fixture.produce().value\n"
    );
    super::box_reference_classes::checked_box(&source, stem)
        .expect("the JVM oracle must establish the virtual getter result");
    match run_native_abi_case(&source, stem) {
        Outcome::Pass => {}
        Outcome::Declined(reason) => assert_eq!(
            reason, "a read of the property `fixture/Base.value`",
            "unsupported dynamic dispatch must remain an explicit property decline"
        ),
        other => panic!("a virtual getter must not call its base entry directly: {other:?}"),
    }
}

// PLATFORM-SPECIFIC: effective finality admits safe entries without requiring every class final.
#[test]
fn native_source_getter_exports_admit_both_forms_of_static_dispatch() {
    let source = include_str!("box_regressions/multimodule/staticallyDispatchedGetterExports.kt");
    let stem = "nativeStaticallyDispatchedGetterExports";
    super::box_reference_classes::checked_box(source, stem)
        .expect("the JVM oracle must establish both static-dispatch results");
    assert_eq!(run_native_abi_case(source, stem), Outcome::Pass);
}
