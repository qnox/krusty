//! What a native plugin's handoff hook may ask about one checked file: where its checked nodes
//! came from, the declarations its calls selected, and the runtime declarations the plugin's
//! generated code calls, selected through the compilation's symbol source.

use crate::backend::{BackendClassifierSource, CheckedBackendCallables};
use crate::diag::Span;
use crate::fir::{ExternalCallableId, ExternalPropertyId, OriginStore, SourceFileId};
use crate::ir::{ExprId, IrFile};
use crate::libraries::FnKind;
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginClassifier, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::plugins::{CheckedSourceFacts, DependencyCallableShape, DependencyDeclaration};
use crate::symbol_source::{SymbolNamespace, SymbolSource};
use crate::types::{Ty, TypeName};

/// One file's checked origins and frozen dependency declarations.
pub(super) struct HandoffSourceFacts<'a> {
    pub(super) source: SourceFileId,
    pub(super) origins: &'a OriginStore,
    pub(super) callables: &'a CheckedBackendCallables,
}

impl CheckedSourceFacts for HandoffSourceFacts<'_> {
    fn expression_span(&self, ir: &IrFile, expression: ExprId) -> Option<Span> {
        let origin = ir.fir_origins.get(&expression)?.fir_origin();
        let (file, span) = self.origins.source_span(origin)?;
        (file == self.source).then_some(span)
    }

    fn dependency_annotations(&self, callable: ExternalCallableId) -> &[TypeName] {
        self.callables
            .callable(callable)
            .map_or(&[], |fact| &fact.annotations)
    }

    fn dependency_callable(
        &self,
        callable: ExternalCallableId,
    ) -> Option<DependencyDeclaration<'_>> {
        let fact = self.callables.callable(callable)?;
        Some(DependencyDeclaration {
            owner: fact.declaration_owner?.name(),
            name: &fact.name,
        })
    }

    fn dependency_property(
        &self,
        property: ExternalPropertyId,
    ) -> Option<DependencyDeclaration<'_>> {
        let fact = self.callables.property(property)?;
        Some(DependencyDeclaration {
            owner: fact.owner,
            name: &fact.name,
        })
    }

    fn dependency_shape(&self, callable: ExternalCallableId) -> Option<DependencyCallableShape> {
        let fact = self.callables.callable(callable)?;
        Some(DependencyCallableShape {
            inline: fact.inline != crate::libraries::InlineKind::None,
            receiver: fact.source_receiver.is_some() || fact.context_count > 0,
            defaults: fact.declares_parameter_defaults,
        })
    }
}

/// The runtime declarations one file's plugins selected, by the declarations the compilation sees.
pub(super) struct HandoffRuntime<'a> {
    source: &'a dyn SymbolSource,
    /// The classifiers of the module and its dependencies, as the backend receives them.
    classifiers: &'a dyn BackendClassifierSource,
    /// Each selected declaration, with the parameters the selecting plugin's transform appends.
    bound: Vec<(ExternalCallableId, Box<[Ty]>)>,
}

impl<'a> HandoffRuntime<'a> {
    pub(super) fn new(
        source: &'a dyn SymbolSource,
        classifiers: &'a dyn BackendClassifierSource,
    ) -> Self {
        Self {
            source,
            classifiers,
            bound: Vec::new(),
        }
    }

    /// The identities selected, for the file's frozen dependency declarations.
    pub(super) fn into_bound(self) -> Vec<(ExternalCallableId, Box<[Ty]>)> {
        self.bound
    }
}

impl PluginRuntime for HandoffRuntime<'_> {
    fn function(&mut self, function: &RuntimeFunction) -> Result<BoundRuntimeFunction, String> {
        // A package declares its top-level functions in its own namespace; a classifier's members
        // are its declared callables, the slot a call on a receiver of that type reads.
        let (declared, kind) = match function.declaration {
            RuntimeDeclaration::Package(package) => (
                self.source
                    .symbols(SymbolNamespace::Package(package), function.name)
                    .callables
                    .clone(),
                FnKind::TopLevel,
            ),
            RuntimeDeclaration::Member(owner) => (
                // The plugin contract names the declaring classifier, so read its normalized
                // declaration table directly.
                self.source
                    .classifier(owner)
                    .and_then(|classifier| {
                        classifier.declared_callables.get(function.name).cloned()
                    })
                    .unwrap_or(crate::libraries::Callables::None),
                FnKind::Member,
            ),
            // A classifier's static functions take no receiver: they live in its own namespace.
            RuntimeDeclaration::Static(owner) => (
                self.source
                    .symbols(SymbolNamespace::Classifier(owner), function.name)
                    .callables
                    .clone(),
                FnKind::TopLevel,
            ),
        };
        let mut selected = declared.functions().iter().filter(|candidate| {
            candidate.kind == kind
                && !candidate.flags.deprecated_hidden
                && candidate.callable.params == function.params
                && candidate.callable.ret == function.ret
        });
        let (Some(candidate), None) = (selected.next(), selected.next()) else {
            return Err(format!(
                "the runtime on the classpath declares no single `{}`",
                function.described()
            ));
        };
        let target = candidate
            .callable
            .external_identity
            .ok_or_else(|| format!("`{}` has no dependency identity", function.described()))?;
        self.bound
            .push((target, function.appended.clone().into_boxed_slice()));
        Ok(BoundRuntimeFunction {
            target,
            params: function.params.clone(),
            ret: function.ret,
        })
    }

    fn appended_parameters(&mut self, target: ExternalCallableId, appended: Vec<Ty>) {
        self.bound.push((target, appended.into_boxed_slice()));
    }

    fn classifier(&self, classifier: TypeName) -> Option<PluginClassifier> {
        let fact = self.classifiers.classifier(classifier)?;
        Some(PluginClassifier {
            kind: fact.kind.into(),
            is_kotlin: fact.is_kotlin || fact.mapped_builtin,
            is_final: !fact.is_extensible,
            value_class: fact.value_declaration.clone(),
            in_module: fact.source,
            is_inner: fact.outer_instance.is_some(),
            type_parameters: fact.own_type_parameter_count,
            supertypes: fact.supertypes.to_vec(),
            annotations: fact.annotations.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        Callables, FunctionInfo, FunctionSet, LibraryCallable, ResolvedSymbols,
    };
    use crate::symbol_source::SymbolNamespace;
    use std::rc::Rc;

    struct Declarations(ResolvedSymbols);

    impl SymbolSource for Declarations {
        fn symbols(&self, _namespace: SymbolNamespace, _name: &str) -> Rc<ResolvedSymbols> {
            Rc::new(self.0.clone())
        }
    }

    struct NoClassifiers;

    impl BackendClassifierSource for NoClassifiers {
        fn classifier(
            &self,
            _classifier: TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            None
        }
    }

    fn runtime(owner: TypeName, target: ExternalCallableId) -> (Declarations, RuntimeFunction) {
        let mut callable =
            LibraryCallable::library(owner, "run", vec![Ty::Int], Ty::Unit, Ty::Unit, "(I)V");
        callable.external_identity = Some(target);
        let candidate = FunctionInfo::plain(FnKind::TopLevel, None, callable);
        (
            Declarations(ResolvedSymbols {
                callables: Callables::Functions(FunctionSet {
                    overloads: vec![candidate],
                }),
                ..ResolvedSymbols::default()
            }),
            RuntimeFunction {
                declaration: RuntimeDeclaration::Package(owner),
                name: "run",
                params: vec![Ty::Int],
                ret: Ty::Unit,
                appended: Vec::new(),
            },
        )
    }

    #[test]
    fn runtime_binding_uses_one_exact_declared_candidate() {
        let owner = crate::types::type_name("example/Runtime");
        let target = ExternalCallableId::from_raw(7);
        let (declared, runtime) = runtime(owner, target);
        let mut handoff = HandoffRuntime::new(&declared, &NoClassifiers);
        assert_eq!(handoff.function(&runtime).unwrap().target, target);
    }

    #[test]
    fn runtime_binding_rejects_ambiguous_exact_declarations() {
        let owner = crate::types::type_name("example/Runtime");
        let (mut declared, runtime) = runtime(owner, ExternalCallableId::from_raw(7));
        let Callables::Functions(functions) = &mut declared.0.callables else {
            unreachable!()
        };
        let mut duplicate = functions.overloads[0].clone();
        duplicate.callable.external_identity = Some(ExternalCallableId::from_raw(8));
        functions.overloads.push(duplicate);
        let mut handoff = HandoffRuntime::new(&declared, &NoClassifiers);
        assert!(handoff.function(&runtime).is_err());
    }
}
