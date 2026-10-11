//! Dependency metadata emitted beside target code.
//!
//! A non-JVM source module publishes a KLIB to later source modules while its target backend emits
//! code for the final program. Both products must observe the same checked IR. This boundary is
//! deliberately read-only: it records metadata and the exact public declaration identities before
//! target representation mutates the file, without cloning or re-resolving the program.

use std::collections::HashMap;

use super::{Artifact, CheckedIrFile};
use crate::diag::DiagSink;
use crate::ir::FunId;
use crate::libraries::KlibDeclarationSignature;

/// Exact public identities of this file's callable realizations.
///
/// The key is a stable common-IR declaration identity, not a spelling. The value is the identity a
/// generated KLIB publishes and a dependent provider later places on `Callee::External`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DependencyExportSignatures {
    callables: HashMap<FunId, KlibDeclarationSignature>,
}

impl DependencyExportSignatures {
    /// The public dependency identity exported for `callable`, when the declaration is visible to
    /// another module and has a callable realization.
    pub fn callable(&self, callable: FunId) -> Option<&KlibDeclarationSignature> {
        self.callables.get(&callable)
    }

    /// Iterate every callable identity this file publishes so a target can install its complete
    /// signature-to-code registry before lowering dependent call sites.
    pub fn callables(
        &self,
    ) -> impl Iterator<Item = (FunId, &KlibDeclarationSignature)> + ExactSizeIterator {
        self.callables
            .iter()
            .map(|(&callable, signature)| (callable, signature))
    }

    /// Record one declaration at the metadata owner that computed its exact identity.
    pub(crate) fn record_callable(&mut self, callable: FunId, signature: KlibDeclarationSignature) {
        let previous = self.callables.insert(callable, signature);
        assert!(
            previous.is_none(),
            "one common-IR callable cannot export two KLIB declaration identities"
        );
    }
}

/// A read-only producer of one source module's dependency metadata.
///
/// A fresh [`Self::State`] belongs to exactly one source module. The target backend state has a
/// longer lifetime and may consume several such modules before it is finalized.
pub trait DependencyMetadata {
    type State: Default;

    /// The source semantics this metadata describes. It must match both the analysis and target
    /// backend that consume it.
    fn compilation_target(&self) -> crate::compilation_target::CompilationTarget;

    /// Observe one checked file before target lowering and return the exact identities its target
    /// code must export. No provider or resolver is reachable through this boundary.
    fn record_ir_file(
        &self,
        file: &CheckedIrFile<'_>,
        state: &mut Self::State,
        diags: &mut DiagSink,
    ) -> DependencyExportSignatures;

    /// Validate the complete source module before its metadata is finalized.
    fn check_module(&self, _state: &Self::State, _diags: &mut DiagSink) {}

    /// Emit this source module's distinct dependency artifact set.
    fn finalize(&self, state: Self::State, module_name: &str) -> Vec<Artifact>;
}
