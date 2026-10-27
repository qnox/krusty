use super::*;

impl SymbolSource for JvmLibraries {
    fn classifier_declaration_name(
        &self,
        declaration: crate::libraries::ClassifierDeclarationKey,
    ) -> Option<Option<crate::types::ClassifierDeclarationName>> {
        use crate::libraries::ClassifierDeclarationKey as Key;
        match declaration {
            Key::TypeAlias(identity) => self
                .cp
                .type_alias_expansion(identity)
                .map(|alias| alias.declaration_name.as_deref().cloned()),
            Key::Ordinary(identity) => {
                let canonical = self
                    .cp
                    .builtin_classifier_name(identity)
                    .unwrap_or(identity);
                self.builtins_customizer
                    .customize(canonical, self.build_library_type(canonical))
                    .map(|record| record.declaration_name)
                    .or_else(|| {
                        self.optional_annotations
                            .classifier(identity)
                            .map(|record| record.declaration_name.clone())
                    })
            }
        }
    }

    /// Both answered by the classpath, which is where this provider interned them.
    fn external_callable(
        &self,
        identity: crate::fir::ExternalCallableId,
    ) -> Option<crate::libraries::ExternalCallableRealization> {
        self.cp.external_callable(identity)
    }

    fn external_property(
        &self,
        identity: crate::fir::ExternalPropertyId,
    ) -> Option<crate::libraries::ExternalPropertyRealization> {
        self.cp.external_property(identity)
    }

    fn package_exists(&self, parent: TypeName, name: &str) -> bool {
        JvmLibraries::package_exists(self, parent, name)
    }

    fn symbols(
        &self,
        namespace: SymbolNamespace,
        name: &str,
    ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
        JvmLibraries::symbols(self, namespace, name)
    }

    fn api_withheld_callable(&self, owner: TypeName, name: &str) -> bool {
        self.api_withheld
            .borrow()
            .get(&owner)
            .is_some_and(|names| names.contains(name))
    }

    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        crate::jvm::jvm_class_map::platform_flexible_upper_bound(lower)
    }

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        classifier_facts::generated_serializer_singleton(self, classifier)
    }

    fn serializable_class_elements(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::SerializableClassElements> {
        classifier_facts::serializable_class_elements(self, classifier)
    }
}

#[cfg(test)]
mod tests;
