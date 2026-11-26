//! What a native plugin's handoff hook may ask about one checked file: where its checked nodes
//! came from, the declarations its calls selected, and the runtime declarations the plugin's
//! generated code calls, selected among the declarations resolution froze for them.

use crate::backend::{BackendClassifierSource, CheckedBackendCallables};
use crate::diag::Span;
use crate::fir::{ExternalCallableId, ExternalPropertyId, OriginStore, SourceFileId};
use crate::ir::{ExprId, IrFile};
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginClassifier, PluginRuntime, RuntimeFunction,
};
use crate::plugins::{CheckedSourceFacts, DependencyCallableShape, DependencyDeclaration};
use crate::resolve::PluginRuntimeInventory;
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

    fn dependency_property_getter(
        &self,
        property: ExternalPropertyId,
    ) -> Option<ExternalCallableId> {
        Some(self.callables.property(property)?.getter)
    }

    fn dependency_shape(&self, callable: ExternalCallableId) -> Option<DependencyCallableShape> {
        let fact = self.callables.callable(callable)?;
        Some(DependencyCallableShape {
            inline: fact.inline != crate::libraries::InlineKind::None,
            extension: fact.source_receiver.is_some(),
            context_parameters: fact.context_count > 0,
            defaults: fact.declares_parameter_defaults,
            synthetic_parameters: synthetic_parameters(&fact.descriptor, &fact.physical_params),
            composable_parameters: fact.composable_parameters,
            inline_modifiers: fact.inline_modifiers.clone(),
        })
    }
}

/// The parameters a compiled entry with `descriptor` takes after `source`, its source parameters.
fn synthetic_parameters(descriptor: &str, source: &[Ty]) -> Box<[Ty]> {
    crate::jvm::jvm_libraries::parse_method_desc(descriptor)
        .and_then(|(entry, _)| entry.strip_prefix(source).map(Box::from))
        .unwrap_or_default()
}

/// The runtime declarations one file's plugins selected, by the declarations the compilation sees.
pub(super) struct HandoffRuntime<'a> {
    runtime: &'a PluginRuntimeInventory,
    /// The classifiers of the module and its dependencies, as the backend receives them.
    classifiers: &'a dyn BackendClassifierSource,
    /// Each selected declaration, with the parameters the selecting plugin's transform appends.
    bound: Vec<(ExternalCallableId, Box<[Ty]>)>,
}

impl<'a> HandoffRuntime<'a> {
    pub(super) fn new(
        runtime: &'a PluginRuntimeInventory,
        classifiers: &'a dyn BackendClassifierSource,
    ) -> Self {
        Self {
            runtime,
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
        let bound = self.runtime.select(function)?;
        self.bound
            .push((bound.target, function.appended.clone().into_boxed_slice()));
        Ok(bound)
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
mod synthetic_parameter_tests {
    use super::synthetic_parameters;
    use crate::types::Ty;

    #[test]
    fn the_parameters_after_the_source_ones_are_synthetic() {
        let tail = synthetic_parameters("(Ljava/lang/String;JII)V", &[Ty::String]);
        assert_eq!(&*tail, &[Ty::Long, Ty::Int, Ty::Int]);
    }

    #[test]
    fn an_entry_that_does_not_begin_with_the_source_parameters_has_none() {
        assert!(synthetic_parameters("(JII)V", &[Ty::String]).is_empty());
        assert!(synthetic_parameters("not a descriptor", &[Ty::String]).is_empty());
    }

    #[test]
    fn an_entry_of_only_the_source_parameters_has_none() {
        assert!(synthetic_parameters("(Ljava/lang/String;)V", &[Ty::String]).is_empty());
    }
}
