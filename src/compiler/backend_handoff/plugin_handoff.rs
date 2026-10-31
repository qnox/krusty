//! What a native plugin's handoff hook may ask about one checked file: where its checked nodes
//! came from, the declarations its calls selected, and the runtime declarations the plugin's
//! generated code calls, selected through the compilation's symbol source.

use crate::backend::CheckedBackendCallables;
use crate::diag::Span;
use crate::fir::{ExternalCallableId, OriginStore, SourceFileId};
use crate::ir::{ExprId, IrFile};
use crate::libraries::FnKind;
use crate::plugins::runtime_declarations::{
    BoundRuntimeFunction, PluginRuntime, RuntimeDeclaration, RuntimeFunction,
};
use crate::plugins::CheckedSourceFacts;
use crate::symbol_resolver::declared_member_callables;
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
}

/// The runtime declarations one file's plugins selected, by the declarations the compilation sees.
pub(super) struct HandoffRuntime<'a> {
    source: &'a dyn SymbolSource,
    /// Each selected declaration, with the parameters the selecting plugin's transform appends.
    bound: Vec<(ExternalCallableId, Box<[Ty]>)>,
}

impl<'a> HandoffRuntime<'a> {
    pub(super) fn new(source: &'a dyn SymbolSource) -> Self {
        Self {
            source,
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
                declared_member_callables(self.source, Ty::obj_name(owner), function.name),
                FnKind::Member,
            ),
        };
        let mut selected = declared.functions().iter().filter(|candidate| {
            candidate.kind == kind
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

    fn declares_classifier(&self, classifier: TypeName) -> bool {
        self.source.classifier(classifier).is_some()
    }
}
