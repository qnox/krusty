//! Target emission of checked source files: the program's dependency support, then each file.
//!
//! A backend that emits library bodies (dependency support) lowers no source file while the
//! program is still being checked. Each module's checked files wait here, with the module state
//! their backend view borrows, until the whole program is known, so dependency support is closed
//! over every file's uses at once. A backend without dependency support lowers each file as soon
//! as it is checked.

use super::{dependency_body_emission, ArtifactOrigin, TargetEmission};
use crate::backend::{
    Artifact, Backend, BackendModuleFacts, CheckedBackendCallables, CheckedBackendClassifiers,
    CheckedIrFile, CheckedIrUnitKind, DependencyExportSignatures,
};
use crate::diag::{DiagSink, Span};
use crate::resolve::PassTwoSymbols;

/// One checked source file, waiting for its target emission.
struct RetainedSourceFile {
    raw_source: usize,
    ir: crate::ir::IrFile,
    source: crate::fir::SourceFileId,
    callables: CheckedBackendCallables,
    dependency_exports: DependencyExportSignatures,
}

/// The module state a checked file's backend view borrows.
struct ModuleState {
    module_name: String,
    stems: Vec<String>,
    symbols: PassTwoSymbols,
    facts: BackendModuleFacts,
    source_map: crate::fir::SourceMap,
}

/// A checked source module whose files wait for target emission, with the module state their
/// backend view borrows.
pub(super) struct CheckedModule {
    state: ModuleState,
    files: Vec<RetainedSourceFile>,
}

/// The checked files of one module, in source order, as Pass 2 hands them off.
#[derive(Default)]
pub(super) struct RetainedFiles(Vec<RetainedSourceFile>);

impl RetainedFiles {
    /// Keep `file`, a checked source file of the module being checked, for target emission.
    pub(super) fn retain(&mut self, raw_source: usize, file: CheckedIrFile<'_>) {
        debug_assert_eq!(file.kind, CheckedIrUnitKind::Source);
        self.0.push(RetainedSourceFile {
            raw_source,
            ir: file.ir,
            source: file.source,
            callables: file.callables,
            dependency_exports: file.dependency_exports,
        });
    }

    /// These files, with the module state their backend view borrows.
    pub(super) fn into_module(
        self,
        module_name: &str,
        stems: &[String],
        symbols: PassTwoSymbols,
        facts: BackendModuleFacts,
        source_map: crate::fir::SourceMap,
    ) -> CheckedModule {
        CheckedModule {
            state: ModuleState {
                module_name: module_name.to_owned(),
                stems: stems.to_vec(),
                symbols,
                facts,
                source_map,
            },
            files: self.0,
        }
    }
}

/// Emit the dependency support every file of `modules` needs, once for the whole program, then
/// each file in module and source order, handing the artifacts to `artifacts`.
pub(super) fn emit_program<B: Backend>(
    modules: Vec<CheckedModule>,
    target: &mut TargetEmission<'_, B>,
    diags: &mut DiagSink,
    artifacts: &mut dyn FnMut(ArtifactOrigin, Artifact),
) {
    let backend = target.backend;
    let (states, files): (Vec<_>, Vec<_>) = modules
        .into_iter()
        .map(|module| (module.state, module.files))
        .unzip();
    let Some(last) = states.last() else {
        return;
    };
    let mut checked = Vec::new();
    for (state, files) in states.iter().zip(files) {
        for retained in files {
            let mut file = CheckedIrFile {
                ir: retained.ir,
                kind: CheckedIrUnitKind::Source,
                source: retained.source,
                classifiers: CheckedBackendClassifiers::new(
                    &state.facts,
                    state.symbols.semantic_platform(),
                ),
                callables: retained.callables,
                dependency_exports: retained.dependency_exports,
                native_plugins: state.symbols.native_plugins(),
                module_name: &state.module_name,
                stems: &state.stems,
                origins: state.source_map.origins(),
            };
            // A target emitting library bodies builds a range class it does not lay out through
            // the library's own declaration, which support emission below then selects.
            diags.set_file(retained.raw_source as u32);
            if let Err(error) = crate::backend::range_constructions::realize(
                &mut file.ir,
                &file.callables,
                &|classifier| backend.represents_dependency_classifier(classifier),
            ) {
                diags.error(Span::new(0, 0), format!("internal error: {error}"));
                return;
            }
            checked.push((retained.raw_source, state, file));
        }
    }
    if backend
        .common_ir_preparation()
        .is_some_and(|preparation| preparation.enums.builds_entries_by_factory())
    {
        let mut programs = checked
            .iter_mut()
            .map(|(_, _, file)| (&mut file.ir, &mut file.dependency_exports))
            .collect::<Vec<_>>();
        crate::backend::enums::withdraw_unread_entries(&mut programs);
    }
    let uses = checked
        .iter()
        .map(|(_, state, file)| dependency_body_emission::DependencyUse {
            file,
            facts: &state.facts,
        })
        .collect::<Vec<_>>();
    dependency_body_emission::emit_dependencies(
        &uses,
        &last.symbols,
        backend,
        target.state,
        target.dependency_support,
        diags,
    )
    .into_iter()
    .for_each(|artifact| artifacts(ArtifactOrigin::DependencySupport, artifact));
    drop(uses);
    for (raw_source, _, file) in checked {
        if diags.has_errors() {
            return;
        }
        diags.set_file(raw_source as u32);
        let initializer_requests = match crate::backend::initializer_declarations(&file) {
            Ok(requests) => requests,
            Err(reason) => {
                diags.error(Span::new(0, 0), format!("internal error: {reason}"));
                return;
            }
        };
        let outputs = backend.lower_ir_file(file, target.state, diags);
        if !diags.has_errors() {
            for declaration in &initializer_requests {
                if backend.emitted_constructor_initializer(target.state, declaration) {
                    target.dependency_support.record_initializer(declaration);
                }
            }
        }
        outputs
            .into_iter()
            .for_each(|artifact| artifacts(ArtifactOrigin::Source(raw_source), artifact));
    }
}

/// Lower `file`, a checked source file of a target that emits no dependency support.
pub(super) fn emit_file<B: Backend>(
    raw_source: usize,
    file: CheckedIrFile<'_>,
    target: &mut TargetEmission<'_, B>,
    diags: &mut DiagSink,
    artifacts: &mut dyn FnMut(ArtifactOrigin, Artifact),
) {
    debug_assert!(!target.backend.emits_dependency_support());
    target
        .backend
        .lower_ir_file(file, target.state, diags)
        .into_iter()
        .for_each(|artifact| artifacts(ArtifactOrigin::Source(raw_source), artifact));
}
