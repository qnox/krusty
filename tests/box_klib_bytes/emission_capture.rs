//! Case-owned KLIB observation from the existing checked-IR emission boundary.
use std::cell::RefCell;

use krusty::backend::{Artifact, CheckedIrFile, DependencyExportSignatures, DependencyMetadata};
use krusty::diag::DiagSink;
use krusty::klib::backend::KlibBackend;

use super::QualifiedArtifacts;

thread_local! {
    static CURRENT: RefCell<Option<Result<QualifiedArtifacts, String>>> = const { RefCell::new(None) };
}

pub(crate) struct Capture {
    enabled: bool,
}
impl Capture {
    pub(super) fn new(enabled: bool) -> Self {
        if enabled {
            CURRENT.with(|current| {
                assert!(current.borrow().is_none(), "nested KLIB case capture");
                *current.borrow_mut() = Some(Ok(QualifiedArtifacts::new()));
            });
        }
        Self { enabled }
    }
    pub(super) fn finish(self) -> Result<QualifiedArtifacts, String> {
        CURRENT
            .with(|current| current.borrow_mut().take())
            .ok_or("KLIB case capture was not enabled".to_string())?
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        if self.enabled {
            CURRENT.with(|current| {
                current.borrow_mut().take();
            });
        }
    }
}

pub(crate) fn active() -> bool {
    CURRENT.with(|current| current.borrow().is_some())
}

pub(crate) fn record(module: &str, artifacts: &[Artifact]) {
    CURRENT.with(|current| {
        let mut current = current.borrow_mut();
        let Some(Ok(entries)) = current.as_mut() else {
            return;
        };
        for (name, bytes) in artifacts {
            if entries
                .insert((module.to_owned(), name.clone()), bytes.clone())
                .is_some()
            {
                *current = Some(Err(format!("duplicate KLIB entry {module}/{name}")));
                return;
            }
        }
    });
}

pub(crate) fn error(message: String) {
    CURRENT.with(|current| {
        if let Some(result @ Ok(_)) = current.borrow_mut().as_mut() {
            *result = Err(message);
        }
    });
}

pub(crate) struct MetadataObserver(pub KlibBackend);
#[derive(Default)]
pub(crate) struct MetadataState {
    module: <KlibBackend as DependencyMetadata>::State,
    diagnostics: RefCell<DiagSink>,
}
impl DependencyMetadata for MetadataObserver {
    type State = MetadataState;
    fn compilation_target(&self) -> krusty::compilation_target::CompilationTarget {
        DependencyMetadata::compilation_target(&self.0)
    }
    fn record_ir_file(
        &self,
        file: &CheckedIrFile<'_>,
        state: &mut Self::State,
        _diags: &mut DiagSink,
    ) -> DependencyExportSignatures {
        self.0
            .record_ir_file(file, &mut state.module, state.diagnostics.get_mut());
        // A single source module has no generated dependency consumer. Observe its library bytes
        // without changing the executable's existing export identities or diagnostics.
        DependencyExportSignatures::default()
    }
    fn check_module(&self, state: &Self::State, _diags: &mut DiagSink) {
        DependencyMetadata::check_module(
            &self.0,
            &state.module,
            &mut state.diagnostics.borrow_mut(),
        );
    }
    fn finalize(&self, state: Self::State, module: &str) -> Vec<Artifact> {
        let diagnostics = state.diagnostics.into_inner();
        if diagnostics.has_errors() {
            error(
                diagnostics
                    .diags
                    .iter()
                    .map(|diagnostic| diagnostic.msg.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        } else {
            record(
                module,
                &DependencyMetadata::finalize(&self.0, state.module, module),
            );
        }
        Vec::new()
    }
}
