//! The current-module symbol provider a checker reads, selected by frontend lifetime.

use super::*;

/// Current-module provider selected by frontend lifetime. Legacy inference runs before signature
/// finalization; production Pass 2 overlays the finalized declaration index and may therefore
/// destroy parser-keyed declaration families before constructing this checker.
pub(super) enum CheckerModuleSymbols<'a> {
    Legacy(crate::module_symbols::ModuleSymbols<'a>),
    Streamed(crate::fir::StreamedModuleSymbols<'a>),
}

impl<'a> CheckerModuleSymbols<'a> {
    /// The temporary signature graph exists only for pre-finalization inference. Callers must
    /// handle its absence explicitly; authoritative Pass 2 has no graph-bearing variant to return.
    pub(super) fn legacy_symbols(&self) -> Option<&'a SymbolTable> {
        match self {
            Self::Legacy(source) => Some(source.pass_one_symbols()),
            Self::Streamed(_) => None,
        }
    }

    pub(super) fn declares_top_level(&self, name: &str) -> bool {
        match self {
            Self::Legacy(source) => source.declares_top_level(name),
            Self::Streamed(source) => source.declares_top_level(name),
        }
    }

    pub(super) fn type_alias_binding(
        &self,
        identity: TypeName,
    ) -> Option<crate::libraries::AliasExpansion> {
        match self {
            Self::Legacy(source) => source.type_alias_binding(identity),
            Self::Streamed(source) => source.type_alias_binding(identity),
        }
    }

    pub(super) fn type_parameter_extra_bounds(&self, identity: &str) -> Vec<Ty> {
        match self {
            Self::Legacy(source) => source.type_parameter_extra_bounds(identity),
            Self::Streamed(source) => source.type_parameter_extra_bounds(identity),
        }
    }

    pub(super) fn type_parameter_bounds(&self, identity: &str) -> Vec<Ty> {
        match self {
            Self::Legacy(source) => source.type_parameter_bounds(identity),
            Self::Streamed(source) => source.type_parameter_bounds(identity),
        }
    }

    pub(super) fn annotation_retention(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::AnnotationRetention> {
        match self {
            Self::Legacy(source) => source.annotation_retention(classifier),
            Self::Streamed(source) => source.annotation_retention(classifier),
        }
    }

    pub(super) fn annotation_targets(&self) -> &'a crate::types::DeclaredTargetPolicies {
        match self {
            Self::Legacy(source) => source.annotation_targets(),
            Self::Streamed(source) => source.annotation_targets(),
        }
    }

    pub(super) fn finalized_function(
        &self,
        declaration: crate::fir::DeclarationId,
        owner: TypeName,
        owner_is_interface: bool,
    ) -> Option<crate::libraries::FunctionInfo> {
        match self {
            Self::Legacy(_) => None,
            Self::Streamed(source) => {
                source.function_for_declaration(declaration, owner, owner_is_interface)
            }
        }
    }
}

impl SymbolSource for CheckerModuleSymbols<'_> {
    fn classifier_declaration_name(
        &self,
        declaration: crate::libraries::ClassifierDeclarationKey,
    ) -> Option<Option<crate::types::ClassifierDeclarationName>> {
        match self {
            Self::Legacy(source) => source.classifier_declaration_name(declaration),
            Self::Streamed(source) => source.classifier_declaration_name(declaration),
        }
    }

    fn actualized_expect(&self, internal: TypeName) -> Option<TypeName> {
        match self {
            Self::Legacy(source) => source.actualized_expect(internal),
            Self::Streamed(source) => source.actualized_expect(internal),
        }
    }

    fn sees_function(&self, function: &crate::libraries::FunctionInfo) -> bool {
        match self {
            Self::Legacy(source) => source.sees_function(function),
            Self::Streamed(source) => source.sees_function(function),
        }
    }

    fn sees_property(&self, property: &crate::libraries::PropertyInfo) -> bool {
        match self {
            Self::Legacy(source) => source.sees_property(property),
            Self::Streamed(source) => source.sees_property(property),
        }
    }

    fn type_variable_bounds(&self, name: &str) -> Vec<Ty> {
        self.type_parameter_bounds(name)
    }

    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        match self {
            Self::Legacy(source) => source.package_exists(parent, name),
            Self::Streamed(source) => source.package_exists(parent, name),
        }
    }

    fn symbols(
        &self,
        namespace: crate::symbol_source::SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        match self {
            Self::Legacy(source) => source.symbols(namespace, name),
            Self::Streamed(source) => source.symbols(namespace, name),
        }
    }

    fn sam_with_receiver_annotations(&self) -> &[TypeName] {
        match self {
            Self::Legacy(source) => source.sam_with_receiver_annotations(),
            Self::Streamed(source) => source.sam_with_receiver_annotations(),
        }
    }
}
