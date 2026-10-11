//! Exact callable identities in one stateful Wasm module.
//!
//! Source-module declarations use the frontend's stable [`CallableId`] while that module is being
//! lowered. Public dependency declarations additionally publish a KLIB signature, which survives
//! independent analysis and is the only key an external call may use. Wasm indices never come
//! from a declaration spelling.

use std::collections::HashMap;

use crate::backend::{CheckedBackendCallables, DependencyExportSignatures};
use crate::fir::{CallableId, ExternalCallableId};
use crate::ir::{Callee, IrExpr, IrFile};
use crate::libraries::KlibDeclarationSignature;

use super::super::encode::Module;
use super::lower::Unsupported;

#[derive(Default)]
pub(super) struct CallableRegistry {
    source_module: Option<String>,
    module: HashMap<CallableId, u32>,
    dependencies: HashMap<KlibDeclarationSignature, u32>,
}

pub(super) struct FileCallTargets {
    module: HashMap<CallableId, u32>,
    external: HashMap<ExternalCallableId, u32>,
}

impl CallableRegistry {
    /// Begin (or continue) one source module. `CallableId` belongs to one analysis and therefore
    /// cannot cross this boundary; exact KLIB signatures and their code remain program-wide.
    pub(super) fn enter_source_module(&mut self, module_name: &str) {
        if self.source_module.as_deref() != Some(module_name) {
            self.source_module = Some(module_name.to_owned());
            self.module.clear();
        }
    }

    /// Declare every function defined by `ir`, reusing an index reserved by an earlier cross-file
    /// call. Every checked callable identity that realizes the same `FunId` binds to that index.
    pub(super) fn declare_file(
        &mut self,
        ir: &IrFile,
        wasm: &mut Module,
    ) -> Result<Vec<u32>, Unsupported> {
        let mut identities = vec![Vec::new(); ir.functions.len()];
        for (&callable, &function) in &ir.checked_callable_functions {
            let Some(targets) = identities.get_mut(function as usize) else {
                return Err("an invalid source callable identity".to_string());
            };
            targets.push(callable);
        }

        identities
            .into_iter()
            .map(|targets| {
                let mut reserved = targets
                    .iter()
                    .filter_map(|target| self.module.get(target).copied());
                let index = reserved.next().unwrap_or_else(|| wasm.declare());
                if reserved.any(|other| other != index) {
                    return Err(
                        "two source callable identities reserved different Wasm functions"
                            .to_string(),
                    );
                }
                for target in targets {
                    self.module.insert(target, index);
                }
                Ok(index)
            })
            .collect()
    }

    /// Resolve every direct cross-file/dependency call before a body is emitted. A source target
    /// may reserve a forward function index; a dependency target must already have been published
    /// by an earlier source module.
    pub(super) fn file_calls(
        &mut self,
        ir: &IrFile,
        callables: &CheckedBackendCallables,
        wasm: &mut Module,
    ) -> Result<FileCallTargets, Unsupported> {
        let mut targets = FileCallTargets {
            module: HashMap::new(),
            external: HashMap::new(),
        };
        for expression in &ir.exprs {
            let IrExpr::Call {
                callee,
                dispatch_receiver,
                ..
            } = expression
            else {
                continue;
            };
            match callee {
                Callee::Module { target, .. } if dispatch_receiver.is_none() => {
                    targets
                        .module
                        .insert(*target, self.module_index(*target, wasm));
                }
                Callee::CrossFile {
                    module_target: Some(target),
                    ..
                } if dispatch_receiver.is_none() => {
                    targets
                        .module
                        .insert(*target, self.module_index(*target, wasm));
                }
                Callee::External {
                    target, defaults, ..
                } if dispatch_receiver.is_none() && defaults.is_empty() => {
                    let signature = callables
                        .callable(*target)
                        .and_then(|callable| callable.declaration_signature.as_ref())
                        .ok_or_else(|| {
                            "a dependency callable without an exact KLIB declaration identity"
                                .to_string()
                        })?;
                    let index = self.dependencies.get(signature).copied().ok_or_else(|| {
                        "a dependency callable whose target code was not emitted".to_string()
                    })?;
                    targets.external.insert(*target, index);
                }
                _ => {}
            }
        }
        Ok(targets)
    }

    /// Publish this file's exact dependency signatures only after its functions have been defined.
    pub(super) fn publish(
        &mut self,
        exports: &DependencyExportSignatures,
        functions: &[u32],
    ) -> Result<(), Unsupported> {
        for (function, signature) in exports.callables() {
            let index = functions
                .get(function as usize)
                .copied()
                .ok_or_else(|| "an invalid exported callable identity".to_string())?;
            match self.dependencies.insert(signature.clone(), index) {
                Some(previous) if previous != index => {
                    return Err("two functions exported one KLIB declaration identity".to_string())
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn module_index(&mut self, target: CallableId, wasm: &mut Module) -> u32 {
        *self.module.entry(target).or_insert_with(|| wasm.declare())
    }
}

impl FileCallTargets {
    pub(super) fn module(&self, target: CallableId) -> Option<u32> {
        self.module.get(&target).copied()
    }

    pub(super) fn external(&self, target: ExternalCallableId) -> Option<u32> {
        self.external.get(&target).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::id_signature::{
        callable_signature, CallableShape, DeclarationContainer, Placement,
    };
    use crate::metadata::semantic::KotlinType;

    fn signature(name: &str) -> KlibDeclarationSignature {
        let package = vec!["demo".to_string()];
        let container = DeclarationContainer::<KotlinType> {
            package: &package,
            classes: &[],
            native_interop_library: false,
        };
        KlibDeclarationSignature::Public(
            callable_signature(
                container,
                &CallableShape {
                    name,
                    contexts: Vec::new(),
                    receiver: None,
                    params: Vec::new(),
                    vararg: None,
                    type_parameters: Vec::new(),
                    expect: false,
                    placement: Placement::Ordinary,
                },
            )
            .expect("a parameterless function has a public signature"),
        )
    }

    #[test]
    fn source_identities_reset_between_modules_but_exact_exports_remain() {
        let mut registry = CallableRegistry::default();
        let mut wasm = Module::default();
        let local = CallableId::from_raw(0);

        registry.enter_source_module("lib");
        let library_index = registry.module_index(local, &mut wasm);
        let signature = signature("answer");
        let mut exports = DependencyExportSignatures::default();
        exports.record_callable(0, signature.clone());
        registry
            .publish(&exports, &[library_index])
            .expect("the library export is valid");

        registry.enter_source_module("app");
        let application_index = registry.module_index(local, &mut wasm);
        assert_ne!(application_index, library_index);
        assert_eq!(registry.dependencies[&signature], library_index);
    }
}
