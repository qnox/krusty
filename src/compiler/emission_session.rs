//! Target emission across separately analyzed source modules.
//!
//! Kotlin module boundaries are frontend boundaries: each module is analyzed against dependency
//! metadata produced by earlier modules. A target program can have a longer lifetime. Native links
//! every module's objects, while Wasm assembles one module and therefore must retain one backend
//! state until the complete source-module graph has been lowered. This session owns that lifetime
//! without flattening source modules into one analysis.

use super::{emit_analyzed_into, Artifact, Backend};
use crate::backend::DependencyMetadata;
use crate::diag::{DiagSink, Span};
use crate::frontend::StreamingSourceSetAnalysis;

/// One source module's dependency metadata, kept distinct so its origin and friend policy can be
/// preserved when a later module selects its libraries.
#[derive(Debug, Eq, PartialEq)]
pub struct EmittedDependencyModule {
    name: String,
    artifacts: Vec<Artifact>,
}

impl EmittedDependencyModule {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    pub fn into_artifacts(self) -> Vec<Artifact> {
        self.artifacts
    }
}

/// One target backend state fed by a sequence of independently analyzed source modules.
pub struct EmissionSession<'a, B: Backend> {
    backend: &'a B,
    state: Option<B::State>,
    dependency_support: super::dependency_body_emission::DependencySupportState,
    target_artifacts: Vec<Artifact>,
    failed: bool,
}

impl<'a, B: Backend> EmissionSession<'a, B> {
    pub fn new(backend: &'a B) -> Self {
        Self {
            backend,
            state: Some(B::State::default()),
            dependency_support: Default::default(),
            target_artifacts: Vec::new(),
            failed: false,
        }
    }

    /// Lower one source module into the retained target state and emit its dependency metadata
    /// from the same checked files. The metadata state is intentionally fresh for this call.
    pub fn emit_module<M: DependencyMetadata>(
        &mut self,
        analysis: impl Into<StreamingSourceSetAnalysis>,
        stems: &[String],
        metadata: &M,
        module_name: &str,
        diags: &mut DiagSink,
    ) -> Option<EmittedDependencyModule> {
        let target = self.backend.compilation_target();
        if metadata.compilation_target() != target {
            diags.error(
                Span::new(0, 0),
                format!(
                    "internal error: dependency metadata for {:?} cannot describe a {target:?} target module",
                    metadata.compilation_target()
                ),
            );
            self.failed = true;
            return None;
        }
        if self.failed {
            diags.error(
                Span::new(0, 0),
                "internal error: cannot emit another module after this target emission session failed",
            );
            return None;
        }

        let mut metadata_state = M::State::default();
        let state = self
            .state
            .as_mut()
            .expect("an unfinished emission session retains its backend state");
        let outputs = emit_analyzed_into(
            analysis,
            stems,
            self.backend,
            state,
            module_name,
            diags,
            &mut self.dependency_support,
            super::PassTwoHooks {
                before_target: &mut |file, diags| {
                    file.dependency_exports =
                        metadata.record_ir_file(file, &mut metadata_state, diags);
                },
                checked_bodies: &mut super::NoObserver,
            },
        );
        if diags.has_errors() {
            self.failed = true;
            return None;
        }
        metadata.check_module(&metadata_state, diags);
        if diags.has_errors() {
            self.failed = true;
            return None;
        }
        self.target_artifacts.extend(outputs);
        Some(EmittedDependencyModule {
            name: module_name.to_string(),
            artifacts: metadata.finalize(metadata_state, module_name),
        })
    }

    /// Validate and finalize the one target program after every source module has been supplied.
    pub fn finish(mut self, module_name: &str, diags: &mut DiagSink) -> Vec<Artifact> {
        if self.failed || diags.has_errors() {
            return Vec::new();
        }
        let state = self
            .state
            .take()
            .expect("an unfinished emission session retains its backend state");
        self.backend.check_module(&state, diags);
        if diags.has_errors() {
            return Vec::new();
        }
        self.target_artifacts
            .extend(self.backend.finalize(state, module_name));
        self.target_artifacts
    }
}

#[cfg(test)]
mod tests;
