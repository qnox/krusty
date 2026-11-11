//! Platform-specific rejection contracts for required KLIB inline dependency templates.

use std::path::{Path, PathBuf};

use krusty::backend::{Artifact, Backend, DependencyMetadata};
use krusty::conformance::TestTarget;
use krusty::diag::DiagSink;
use krusty::frontend::StreamingSourceSetAnalysis;
use krusty::klib::write::{KlibPlatform, KlibStamp};
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use krusty::libraries::{
    ExternalCallableRealization, FnKind, FunctionInfo, KlibDeclarationSignature,
    KlibDependencyCallable,
};
use krusty::symbol_source::{SymbolNamespace, SymbolSource};
use krusty::types::{SemanticCallableOwner, Ty, TyKind, TypeName};
use krusty_native::NativeTarget;

#[derive(Clone, Copy)]
pub(super) struct Case {
    pub(super) stem: &'static str,
    pub(super) source: &'static str,
}

/// A library's reified inline function whose body the library serializes no template for (its
/// `for` loop). The declaration has no legal direct call, so a target that expands dependency
/// inline bodies in IR must reject the consumer rather than call it.
pub(super) const DECLINED_REQUIRED_INLINE_BODY: Case = Case {
    stem: "multimoduleDeclinedRequiredInlineBody",
    source: "// MODULE: lib\n\
             inline fun <reified T> countOf(value: Any, times: Int): Int {\n\
             \x20   var count = 0\n\
             \x20   for (index in 0 until times) if (value is T) count++\n\
             \x20   return count\n\
             }\n\
             // MODULE: main(lib)\n\
             fun box(): String = if (countOf<String>(\"a\", 2) == 2) \"OK\" else \"fail\"\n",
};

/// A required inline factory whose prepared body contains a rich function reference, which
/// template lowering does not yet support. Neither target may emit an illegal direct call to
/// the factory.
pub(super) const DECLINED_STDLIB_INLINE_BODY: &str = "fun box(): String {\n\
                                               \x20   val order = compareBy<String> { it }\n\
                                               \x20   return if (order.compare(\"a\", \"b\") < 0) \"OK\" else \"fail\"\n\
                                               }\n";

/// Establish that the platform-specific rejection fixture is an ordinary valid Kotlin program.
pub(super) fn verify_declined_stdlib_source_with_kotlinc() {
    let stdlib = super::common::stdlib_jar();
    let reference = super::box_reference_classes::reference_compile(
        DECLINED_STDLIB_INLINE_BODY,
        "declinedRequiredStdlibTemplate",
        std::slice::from_ref(&stdlib),
        krusty::conformance::BoxJdk::Full { root: None },
        "",
    )
    .expect("pinned kotlinc must compile the required-inline rejection fixture");
    let classes = reference.flatten().into_iter().collect::<Vec<_>>();
    let entry = super::common::find_box_class(&classes)
        .expect("the required-inline rejection fixture declares box()");
    assert_eq!(
        super::common::run_box(&classes, &entry, &[stdlib]),
        Some("OK".to_owned()),
        "pinned kotlinc must run the required-inline rejection fixture"
    );
}

/// One target's pinned KLIB stdlib, plus the Native host when that target is Native.
#[derive(Clone, Copy)]
pub(super) struct DependencyTarget {
    test_target: TestTarget,
    native: Option<NativeTarget>,
}

impl DependencyTarget {
    pub(super) fn native(target: NativeTarget) -> Self {
        Self {
            test_target: TestTarget::Native,
            native: Some(target),
        }
    }

    pub(super) fn wasm(target: TestTarget) -> Self {
        assert!(
            matches!(target, TestTarget::WasmJs | TestTarget::WasmWasi),
            "a Wasm inline-rejection target is wasm-js or wasm-wasi"
        );
        Self {
            test_target: target,
            native: None,
        }
    }
}

/// Owner, source name, declaration signature, and module-qualified dependency of one selected
/// callable. The compilation-local [`krusty::fir::ExternalCallableId`] is not part of this
/// contract: a provider assigns it per compilation, while these four facts are the declaration.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SelectedDependencyContract {
    owner: Option<SemanticCallableOwner>,
    name: String,
    declaration: Option<KlibDeclarationSignature>,
    dependency: Option<KlibDependencyCallable>,
}

impl SelectedDependencyContract {
    fn from_realization(realization: &ExternalCallableRealization) -> Self {
        let contract = Self {
            owner: realization.declaration_owner,
            name: realization.callable.name.clone(),
            declaration: realization.declaration_signature.clone(),
            dependency: realization.dependency_callable.clone(),
        };
        assert!(
            contract.owner.is_some()
                && !contract.name.is_empty()
                && contract.declaration.is_some()
                && contract
                    .dependency
                    .as_ref()
                    .is_some_and(|dependency| !dependency.module.is_empty()),
            "the selected declaration publishes its owner, source name, signature, and module"
        );
        contract
    }

    /// The diagnostic tail `selected_external_call_diagnostic` appends for this declaration.
    fn diagnostic_suffix(&self) -> String {
        format!(
            "; selected dependency owner={:?}, name={:?}, declaration={:?}, dependency={:?}",
            self.owner, self.name, self.declaration, self.dependency,
        )
    }
}

/// The frontend rejection of a call whose required inline dependency has no template.
///
/// `expected` is the provider's typed declaration. The message must carry that owner, source
/// name, declaration signature, and module-qualified dependency exactly. Only the
/// compilation-local callable id is read back as a number.
pub(super) fn required_inline_rejection(
    message: &str,
    expected: &SelectedDependencyContract,
) -> Option<u32> {
    let rest = message.strip_prefix(
        "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(",
    )?;
    let (id, rest) = rest.split_once(")))")?;
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if rest != expected.diagnostic_suffix() {
        return None;
    }
    id.parse().ok()
}

/// The fixture `lib` module's `countOf`, as that module's produced KLIB publishes it.
pub(super) fn count_of_from_lib_producer(target: DependencyTarget) -> SelectedDependencyContract {
    let archive = produce_lib_archive(target);
    let libraries = open_provider(target, std::slice::from_ref(&archive.path));
    let contract = exactly_one(&libraries, TypeName::ROOT, "countOf");
    assert_eq!(
        contract
            .dependency
            .as_ref()
            .map(|dependency| dependency.module.as_str()),
        Some("lib"),
        "countOf belongs to the fixture's lib module"
    );
    contract
}

/// The `compareBy` overload `compareBy<String> { it }` selects from this target's stdlib.
pub(super) fn selected_compare_by(target: DependencyTarget) -> SelectedDependencyContract {
    compare_by_overloads(target).selected
}

struct CompareByOverloads {
    selected: SelectedDependencyContract,
    other: SelectedDependencyContract,
}

fn compare_by_overloads(target: DependencyTarget) -> CompareByOverloads {
    let libraries = open_provider(target, &[]);
    let symbols = libraries.symbols(
        SymbolNamespace::Package(krusty::types::type_name("kotlin/comparisons")),
        "compareBy",
    );
    let functions = symbols.callables.functions();
    let mut selected = None;
    let mut other = None;
    let mut described = Vec::new();
    for function in functions {
        let contract = realization_of(&libraries, function);
        described.push(format!(
            "kind={:?} inline={:?} vararg={} params={:?}",
            function.kind,
            function.callable.inline,
            function.call_sig.vararg,
            function.callable.params,
        ));
        if is_selected_compare_by(function) {
            assert!(
                selected.replace(contract).is_none(),
                "compareBy<String> {{ it }} selects one overload, found {described:?}"
            );
        } else if other.is_none() {
            other = Some(contract);
        }
    }
    let selected = selected.unwrap_or_else(|| {
        panic!("kotlin.comparisons.compareBy has no single-selector overload: {described:?}")
    });
    let other = other.unwrap_or_else(|| {
        panic!("kotlin.comparisons.compareBy has no second overload: {described:?}")
    });
    assert_eq!(selected.name, "compareBy");
    assert_eq!(
        selected.owner,
        Some(SemanticCallableOwner::Package(krusty::types::type_name(
            "kotlin/comparisons"
        )))
    );
    assert_eq!(selected.owner, other.owner);
    assert_eq!(selected.name, other.name);
    assert_ne!(selected.declaration, other.declaration);
    assert_eq!(
        selected
            .dependency
            .as_ref()
            .map(|dependency| dependency.module.as_str()),
        other
            .dependency
            .as_ref()
            .map(|dependency| dependency.module.as_str()),
        "both compareBy overloads come from the same stdlib module"
    );
    CompareByOverloads { selected, other }
}

/// `compareBy<String> { it }` is the inline, non-vararg, single function-parameter overload.
fn is_selected_compare_by(function: &FunctionInfo) -> bool {
    let [parameter] = function.callable.params.as_slice() else {
        return false;
    };
    function.kind == FnKind::TopLevel
        && function.receiver.is_none()
        && !function.call_sig.vararg
        && function.callable.inline.can_inline()
        && is_one_argument_function(*parameter)
}

fn is_one_argument_function(ty: Ty) -> bool {
    let inner = match ty.kind() {
        TyKind::Nullable(inner) | TyKind::PlatformNullable(inner) => inner.kind(),
        kind => kind,
    };
    matches!(inner, TyKind::Fun(signature) if signature.params.len() == 1 && !signature.suspend)
}

fn exactly_one(
    libraries: &KlibLibraries,
    owner: TypeName,
    name: &str,
) -> SelectedDependencyContract {
    let symbols = libraries.symbols(SymbolNamespace::Package(owner), name);
    let [function] = symbols.callables.functions() else {
        panic!(
            "expected one `{name}`, found {}",
            symbols.callables.functions().len()
        );
    };
    let contract = realization_of(libraries, function);
    assert_eq!(contract.name, name);
    contract
}

fn realization_of(
    libraries: &KlibLibraries,
    function: &FunctionInfo,
) -> SelectedDependencyContract {
    let identity = function
        .callable
        .external_identity
        .expect("the provider assigns the declaration an identity");
    let realization = libraries
        .external_callable(identity)
        .expect("the provider realizes its assigned identity");
    SelectedDependencyContract::from_realization(&realization)
}

struct ProducedArchive {
    path: PathBuf,
}

impl Drop for ProducedArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn produce_lib_archive(target: DependencyTarget) -> ProducedArchive {
    let source = DECLINED_REQUIRED_INLINE_BODY.source;
    let modules = krusty::conformance::split_modules(source)
        .expect("the required-inline fixture splits into modules");
    let units = krusty::conformance::module_units(&modules);
    let lib = units
        .iter()
        .find(|unit| unit.name == "lib")
        .expect("the fixture declares module lib");
    let mut diagnostics = DiagSink::new();
    let (analysis, stems, _) = super::box_lane::analyze_module(
        lib,
        &[],
        &[],
        source,
        super::box_lane::Libraries::StdlibKlib,
        target.test_target,
        target.test_target.compilation_target(),
        &mut diagnostics,
    )
    .unwrap_or_else(|outcome| panic!("the lib producer did not analyze: {outcome:?}"));
    let features = krusty::features::LangFeatures::from_source(source);
    let stamp = KlibStamp::new(
        krusty::kotlin_version::target(),
        features.language_version(),
    );
    let metadata = klib_metadata(target, stamp, features.has("AnnotationsInMetadata"));
    let artifacts = match target.test_target {
        TestTarget::Native => {
            let backend =
                krusty_native::CraneliftBackend::new(target.native.expect("Native producer"))
                    .with_entry(krusty::backend::Entry::Box)
                    .with_entry_module("main")
                    .verified();
            emit_archive(
                &backend,
                &metadata,
                analysis,
                &stems,
                &lib.name,
                &mut diagnostics,
            )
        }
        TestTarget::WasmJs => {
            let backend = krusty_wasm::WasmBackend::new(krusty_wasm::WasmTarget::Js)
                .with_entry(krusty::backend::Entry::Box)
                .with_entry_module("main");
            emit_archive(
                &backend,
                &metadata,
                analysis,
                &stems,
                &lib.name,
                &mut diagnostics,
            )
        }
        TestTarget::WasmWasi => {
            let backend = krusty_wasm::WasmBackend::new(krusty_wasm::WasmTarget::Wasi)
                .with_entry(krusty::backend::Entry::Box)
                .with_entry_module("main");
            emit_archive(
                &backend,
                &metadata,
                analysis,
                &stems,
                &lib.name,
                &mut diagnostics,
            )
        }
        TestTarget::Jvm => panic!("a KLIB inline rejection has no JVM producer"),
    };
    ProducedArchive {
        path: write_archive(&artifacts),
    }
}

fn klib_metadata(
    target: DependencyTarget,
    stamp: KlibStamp,
    annotations: bool,
) -> krusty::klib::backend::KlibBackend {
    let platform = match target.test_target {
        TestTarget::Native => KlibPlatform::Native,
        TestTarget::WasmJs => KlibPlatform::WasmJs,
        TestTarget::WasmWasi => KlibPlatform::WasmWasi,
        TestTarget::Jvm => panic!("a KLIB inline rejection has no JVM producer"),
    };
    let metadata = krusty::klib::backend::KlibBackend::new(platform, stamp)
        .with_annotations_in_metadata(annotations);
    match target.native {
        Some(native) => metadata
            .with_native_target(native)
            .expect("the Native producer binds its host target"),
        None => metadata,
    }
}

fn emit_archive<B: Backend, M: DependencyMetadata>(
    backend: &B,
    metadata: &M,
    analysis: StreamingSourceSetAnalysis,
    stems: &[String],
    module_name: &str,
    diagnostics: &mut DiagSink,
) -> Vec<Artifact> {
    let mut session = krusty::compiler::EmissionSession::new(backend);
    let emitted = session
        .emit_module(analysis, stems, metadata, module_name, diagnostics)
        .unwrap_or_else(|| {
            panic!(
                "the lib producer did not emit: {}",
                diagnostics
                    .diags
                    .iter()
                    .map(|diagnostic| diagnostic.msg.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    assert_eq!(emitted.name(), module_name);
    emitted.into_artifacts()
}

fn write_archive(artifacts: &[Artifact]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "krusty-inline-rejection-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    std::fs::create_dir_all(&root).expect("create the produced KLIB directory");
    for (name, bytes) in artifacts {
        let relative = Path::new(name);
        assert!(
            relative.is_relative()
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_))),
            "produced KLIB artifact path {name:?}"
        );
        let output = root.join(relative);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).expect("create a produced KLIB directory");
        }
        std::fs::write(output, bytes).expect("write a produced KLIB artifact");
    }
    root
}

fn open_provider(target: DependencyTarget, dependencies: &[PathBuf]) -> KlibLibraries {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(target.test_target.compilation_target())
        .expect("the pinned target stdlib KLIB");
    let semantics = match target.test_target {
        TestTarget::Native => KlibStdlibSemantics::Native,
        TestTarget::WasmJs | TestTarget::WasmWasi => KlibStdlibSemantics::Wasm,
        TestTarget::Jvm => panic!("a KLIB inline rejection has no JVM provider"),
    };
    let mut inputs = dependencies
        .iter()
        .cloned()
        .map(KlibLibraryInput::dependency)
        .collect::<Vec<_>>();
    inputs.push(KlibLibraryInput::platform_stdlib(stdlib, semantics));
    if let Some(native) = target.native {
        krusty_native::NativeKlibInputs::open(&inputs, native)
            .unwrap_or_else(|error| panic!("opening the Native provider: {error}"))
            .into_parts()
            .0
    } else {
        KlibLibraries::open(&inputs)
            .unwrap_or_else(|error| panic!("opening the KLIB provider: {error}"))
    }
}

fn provisioned_target() -> Option<DependencyTarget> {
    if let Some(native) = NativeTarget::host() {
        if krusty_native::can_link(native)
            && krusty::toolchain::kotlin_stdlib_klib(TestTarget::Native.compilation_target())
                .is_some()
        {
            return Some(DependencyTarget::native(native));
        }
    }
    [TestTarget::WasmJs, TestTarget::WasmWasi]
        .into_iter()
        .find(|target| krusty::toolchain::kotlin_stdlib_klib(target.compilation_target()).is_some())
        .map(DependencyTarget::wasm)
}

#[cfg(test)]
mod tests {
    use super::{
        compare_by_overloads, count_of_from_lib_producer, provisioned_target,
        required_inline_rejection, SelectedDependencyContract,
    };

    fn rejection(contract: &SelectedDependencyContract, id: u32) -> String {
        format!(
            "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId({id}))){}",
            contract.diagnostic_suffix()
        )
    }

    #[test]
    fn a_required_inline_rejection_matches_only_its_selected_dependency() {
        let Some(target) = provisioned_target() else {
            if std::env::var_os("KRUSTY_REQUIRE_NATIVE_CONFORMANCE").is_some()
                || std::env::var_os("KRUSTY_REQUIRE_WASM_JS_CONFORMANCE").is_some()
                || std::env::var_os("KRUSTY_REQUIRE_WASM_WASI_CONFORMANCE").is_some()
            {
                panic!("the selected-dependency contract needs its pinned target stdlib");
            }
            return;
        };
        let count_of = count_of_from_lib_producer(target);
        let compare_by = compare_by_overloads(target);
        assert_eq!(
            required_inline_rejection(&rejection(&count_of, 7), &count_of),
            Some(7)
        );
        assert_eq!(
            required_inline_rejection(&rejection(&count_of, 88), &count_of),
            Some(88),
            "only the compilation-local callable id is normalized"
        );
        assert_eq!(
            required_inline_rejection(&rejection(&compare_by.selected, 13), &compare_by.selected),
            Some(13)
        );

        let mut wrong_owner = count_of.clone();
        wrong_owner.owner = compare_by.selected.owner;
        assert_ne!(wrong_owner.owner, count_of.owner);
        assert_eq!(
            required_inline_rejection(&rejection(&count_of, 7), &wrong_owner),
            None,
            "a different owner is not countOf"
        );

        let mut wrong_overload = compare_by.selected.clone();
        wrong_overload.declaration = compare_by.other.declaration.clone();
        wrong_overload
            .dependency
            .as_mut()
            .expect("compareBy publishes its module")
            .declaration = compare_by
            .other
            .declaration
            .clone()
            .expect("the other overload publishes its signature");
        assert_eq!(
            required_inline_rejection(&rejection(&compare_by.selected, 13), &wrong_overload),
            None,
            "another compareBy overload is not the selected selector"
        );

        let mut wrong_library = count_of.clone();
        wrong_library
            .dependency
            .as_mut()
            .expect("countOf publishes its module")
            .module = compare_by
            .selected
            .dependency
            .as_ref()
            .expect("compareBy publishes its module")
            .module
            .clone();
        assert_ne!(
            wrong_library
                .dependency
                .as_ref()
                .map(|dependency| &dependency.module),
            count_of
                .dependency
                .as_ref()
                .map(|dependency| &dependency.module)
        );
        assert_eq!(
            required_inline_rejection(&rejection(&count_of, 7), &wrong_library),
            None,
            "the stdlib module is not the fixture lib"
        );

        let missing_tail = "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(7)))";
        assert_eq!(
            required_inline_rejection(missing_tail, &count_of),
            None,
            "a missing selected-dependency tail is not the declaration"
        );
        assert_eq!(
            required_inline_rejection(&format!("{missing_tail}; selected dependency "), &count_of),
            None
        );
        assert_eq!(
            required_inline_rejection(
                "internal error: checked FIR lowering failed: Body(UnsupportedExternalCall(ExternalCallableId(nope)))",
                &count_of
            ),
            None
        );
        assert_eq!(
            required_inline_rejection("a different diagnostic", &count_of),
            None
        );
        assert_eq!(
            required_inline_rejection(&rejection(&compare_by.selected, 13), &count_of),
            None,
            "compareBy does not satisfy the countOf contract"
        );
    }
}
