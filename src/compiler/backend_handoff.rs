//! The frontend/backend boundary of one checked file: freeze what its backend may read.
//!
//! A backend receives the file's common IR, never the frontend symbol table. Dependency callables
//! the IR selected arrive as a frozen copy of their provider's normalized facts, so a backend has
//! no reason to ask the provider about one. Classifiers are not frozen yet: the classifier view
//! still answers a dependency classifier through the provider.

use crate::backend::{
    BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers, CheckedIrFile,
    CheckedIrUnitKind,
};
use crate::diag::{DiagSink, Span};
use crate::fir::SourceFileId;
use crate::resolve::PassTwoSymbols;

mod plugin_handoff;

/// The module-wide facts every file of one emission hands its backend alongside its own IR.
pub(super) struct ModuleEmission<'a> {
    pub module_name: &'a str,
    /// The text of the file being handed off, for plugins that record source facts.
    pub text: &'a str,
    /// Each source file's output stem, by source identity.
    pub stems: &'a [String],
    /// Where each checked node came from, for a backend to place what it reports.
    pub origins: &'a crate::fir::OriginStore,
    /// The backend's common-IR preparation policy includes the shared coroutine passes.
    pub common_ir_coroutines: bool,
    /// The dependency classes whose supertype members the backend dispatches itself
    /// ([`crate::backend::Backend::dispatched_dependency_classes`]).
    pub dispatched_classes: &'a [crate::types::TypeName],
    /// The backend appends a `CharSequence` to a `StringBuilder` through its `length` and `get`
    /// ([`crate::backend::Backend::appends_char_sequences_by_index`]).
    pub appends_char_sequences_by_index: bool,
    /// The backend's own list reads another collection through its interface members
    /// ([`crate::backend::Backend::reads_collections_through_interfaces`]).
    pub collections_through_interfaces: bool,
}

/// Freeze `ir`'s backend view, or report an internal error when a dependency identity the IR holds
/// has no provider answer.
pub(super) fn checked_ir_file<'a>(
    completed: super::CompletedSourceIr,
    source: SourceFileId,
    module_facts: &'a BackendModuleFacts,
    symbols: &'a PassTwoSymbols,
    module: ModuleEmission<'a>,
    diags: &mut DiagSink,
) -> Option<CheckedIrFile<'a>> {
    let mut ir = completed.ir;
    let classifiers = CheckedBackendClassifiers::new(module_facts, symbols.semantic_platform());
    // Plugin-generated declarations are part of the file every target receives.
    crate::plugins::declare_enabled(
        &mut ir,
        symbols.native_plugins(),
        module.module_name,
        module_facts.serialization_generated_member_statuses(),
        &classifiers,
    );
    // A suspend function's lowering calls two stdlib declarations the source need not name; select
    // them now from the identities resolution stored before backend entry.
    if module.common_ir_coroutines && !ir.suspend_funs.is_empty() {
        ir.coroutine_runtime = symbols.coroutine_runtime();
    }
    let mut callables = match CheckedBackendCallables::freeze_with_class_declarations(
        &ir,
        symbols.semantic_platform(),
        module_facts.classes(),
    ) {
        Ok(callables) => callables,
        Err(error) => {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot freeze dependency declaration facts: {error:?}"),
            );
            return None;
        }
    };
    if module.appends_char_sequences_by_index {
        if let Some(protocol) = symbols.char_sequence_protocol() {
            if let Err(error) =
                callables.freeze_char_sequence_protocol(protocol, symbols.semantic_platform())
            {
                diags.error(
                    Span::new(0, 0),
                    format!("internal error: cannot freeze the CharSequence members: {error:?}"),
                );
                return None;
            }
        }
    }
    if module.collections_through_interfaces {
        if let Err(error) = freeze_collection_protocol(&mut callables, symbols.semantic_platform())
        {
            diags.error(
                Span::new(0, 0),
                format!("internal error: cannot freeze the collection members: {error:?}"),
            );
            return None;
        }
    }
    if let Err(error) = record_checked_klib_dependency_facts(
        &mut callables,
        symbols.semantic_platform(),
        module.dispatched_classes,
    ) {
        diags.error(Span::new(0, 0), format!("internal error: {error}"));
        return None;
    }
    if let Err(identity) = super::builtin_implementations::record_checked(
        &mut callables,
        &ir,
        crate::resolve::RepresentedOverrides::new(
            symbols.semantic_platform(),
            module.dispatched_classes,
        ),
    ) {
        diags.error(
            Span::new(0, 0),
            format!(
                "internal error: checked IR has no builtin implementation facts for external member {identity:?}"
            ),
        );
        return None;
    }
    let file_name = module
        .stems
        .get(source.raw() as usize)
        .map(|stem| format!("{}.kt", stem.rsplit(['/', '\\']).next().unwrap_or(stem)))
        .unwrap_or_default();
    let mut runtime =
        plugin_handoff::HandoffRuntime::new(symbols.semantic_platform(), &classifiers);
    let rejections = crate::plugins::record_enabled_source_facts(
        &mut ir,
        symbols.native_plugins(),
        module.module_name,
        &crate::plugins::PluginSourceView {
            text: module.text,
            file_name: &file_name,
            source,
            checked: &plugin_handoff::HandoffSourceFacts {
                source,
                origins: module.origins,
                callables: &callables,
            },
        },
        &mut runtime,
    );
    if !rejections.is_empty() {
        for rejection in rejections {
            let kind = if rejection.refusal {
                crate::diag::DiagnosticKind::PluginRefusal
            } else {
                crate::diag::DiagnosticKind::Compiler
            };
            diags.error_kind(rejection.span, kind, rejection.message);
        }
        return None;
    }
    // The runtime declarations the plugins' generated code calls join the file's frozen ones.
    let provider = symbols.semantic_platform();
    if let Err(error) = callables
        .freeze_plugin_runtime_callables(runtime.into_bound(), |identity| {
            provider.external_callable(identity)
        })
    {
        diags.error(
            Span::new(0, 0),
            format!("internal error: cannot freeze plugin runtime declaration facts: {error:?}"),
        );
        return None;
    }
    super::capture_storage::retire(
        &mut ir,
        symbols.native_plugins(),
        &completed.local_storage_owners,
    );
    // Each class's declarations are recorded here, before any target lowers them.
    crate::metadata::class_declarations::record_all(&mut ir);
    // Common code typed an `expect` classifier as its own; every target realizes the classifier
    // the `actual typealias` actualizing it names, so the file names that one from here on.
    let (expects, nested_expects) = module_facts.actualized_expects();
    ir.actualize_expects(expects, nested_expects);
    Some(CheckedIrFile {
        ir,
        kind: CheckedIrUnitKind::Source,
        source,
        classifiers,
        callables,
        dependency_exports: Default::default(),
        native_plugins: symbols.native_plugins(),
        module_name: module.module_name,
        stems: module.stems,
        origins: module.origins,
    })
}

/// Check the KLIB body graph reachable from the frozen callables and store its facts, before the
/// checked callable facts are completed. A refreeze after common preparation checks the graph of
/// the calls the preparation introduced the same way, and a dependency support unit checks the
/// graph its own bodies reach.
pub(super) fn record_checked_klib_dependency_facts(
    callables: &mut CheckedBackendCallables,
    platform: &dyn crate::libraries::SemanticPlatform,
    represented: &[crate::types::TypeName],
) -> Result<(), String> {
    let Some(dependency_bodies) = platform.klib_dependency_body_facts() else {
        return Ok(());
    };
    let checked = crate::resolve::for_checked_klib_bodies(
        platform,
        dependency_bodies.bodies(),
        callables.klib_body_callables(),
        represented,
    )
    .map_err(|identity| {
        format!("checked KLIB body graph has no exact member facts for {identity:?}")
    })?;
    callables
        .record_klib_dependency_facts(checked)
        .map_err(|identity| {
            format!("checked KLIB body graph has conflicting facts for {identity:?}")
        })
}

/// Run common IR preparations after the target's exact dependency exports have been attached, but
/// before target lowering computes its emission closure. `provider` remains live here, so the
/// final callable freeze includes calls introduced by common lowering. Dependency-support units
/// can use this entry point after their checked unit and exact exports have been constructed.
pub(super) fn prepare_common_ir(
    file: &mut CheckedIrFile<'_>,
    provider: &dyn crate::libraries::SemanticPlatform,
    preparation: crate::backend::CommonIrPreparation,
    coroutine_runtime: Option<crate::ir::IrCoroutineRuntime>,
    represented: &[crate::types::TypeName],
) -> Result<(), crate::backend::CommonIrPreparationError> {
    // Resolution selected the coroutine runtime declarations once; a unit whose preparation
    // lowers a suspend function records that saved selection.
    if !file.ir.suspend_funs.is_empty() && file.ir.coroutine_runtime.is_none() {
        file.ir.coroutine_runtime = coroutine_runtime;
    }
    if preparation.property_reference_values {
        crate::backend::property_reference_values::realize(&mut file.ir);
    }
    crate::backend::counted_loops::realize(&mut file.ir, preparation.counted_loops);
    crate::backend::enums::publish_constants(&mut file.ir, &mut file.dependency_exports);
    crate::backend::enums::realize(
        &mut file.ir,
        preparation.enums,
        &mut file.dependency_exports,
    );
    let classifiers = file.classifiers.module();
    let module_name = file.module_name;
    crate::backend::default_arguments::realize(
        &mut file.ir,
        &mut file.callables,
        |function| file.dependency_exports.callable(function).is_some(),
        |constructor| {
            classifiers.declares_instance(
                &crate::backend::instance_links::InstanceEntry::Constructor {
                    module: module_name.into(),
                    declaration: constructor,
                },
            )
        },
    );
    crate::backend::coroutines::add_continuation_parameters(&mut file.ir)
        .map_err(crate::backend::CommonIrPreparationError::Suspend)?;

    let plugin_runtime = file.callables.plugin_runtime_bindings();
    let char_sequence_protocol = file.callables.char_sequence_protocol();
    let collection_protocol = file.callables.collection_protocol();
    // The refreeze keeps the declarations of the module's classes, as the handoff froze them:
    // the slot plan binds a dependency superclass's published layout by those declarations.
    let mut callables = CheckedBackendCallables::freeze_with_class_declarations(
        &file.ir,
        provider,
        file.classifiers.module().classes(),
    )
    .map_err(|error| {
        crate::backend::CommonIrPreparationError::Invariant(format!(
            "cannot freeze prepared dependency declarations: {error:?}"
        ))
    })?;
    // The handoff froze the recorded CharSequence selection; the refreeze keeps it, before the
    // checked KLIB graph is recorded, as the handoff orders them.
    if let Some(protocol) = char_sequence_protocol {
        callables
            .freeze_char_sequence_protocol(protocol, provider)
            .map_err(|error| {
                crate::backend::CommonIrPreparationError::Invariant(format!(
                    "cannot refreeze the CharSequence members: {error:?}"
                ))
            })?;
    }
    if let Some(protocol) = collection_protocol {
        callables
            .freeze_collection_protocol(protocol, provider)
            .map_err(|error| {
                crate::backend::CommonIrPreparationError::Invariant(format!(
                    "cannot refreeze the collection members: {error:?}"
                ))
            })?;
    }
    record_checked_klib_dependency_facts(&mut callables, provider, represented)
        .map_err(crate::backend::CommonIrPreparationError::Invariant)?;
    super::builtin_implementations::record_checked(
        &mut callables,
        &file.ir,
        crate::resolve::RepresentedOverrides::new(provider, represented),
    )
    .map_err(|identity| {
        crate::backend::CommonIrPreparationError::Invariant(format!(
            "prepared IR has no builtin implementation facts for external member {identity:?}"
        ))
    })?;
    callables
        .freeze_plugin_runtime_callables(plugin_runtime, |identity| {
            provider.external_callable(identity)
        })
        .map_err(|error| {
            crate::backend::CommonIrPreparationError::Invariant(format!(
                "cannot refreeze plugin runtime declaration facts: {error:?}"
            ))
        })?;
    file.callables = callables;
    Ok(())
}

/// Select and freeze the collection members a target's own list reads another collection through.
/// A provider declaring no single one of them freezes nothing, and a list that needs them declines.
pub(super) fn freeze_collection_protocol(
    callables: &mut CheckedBackendCallables,
    provider: &dyn crate::symbol_source::SymbolSource,
) -> Result<(), crate::backend::DependencyFactError> {
    match crate::resolve::collection_protocol::selected(provider) {
        Some(protocol) => callables.freeze_collection_protocol(protocol, provider),
        None => Ok(()),
    }
}
