//! The non-local Kotlin classifiers a source module publishes to foreign-language providers.

use super::{
    DeclarationFlags, DeclarationKind, DeclarationStub, HeaderDeclarationKind, StreamedHeaderModule,
};
use crate::types::{ClassifierDeclarationName, TypeName};

impl StreamedHeaderModule {
    /// The exact non-local declaration name, before parser hoisting qualifies lookup spellings.
    pub(crate) fn classifier_declaration_name(
        &self,
        stub: &DeclarationStub,
    ) -> Option<ClassifierDeclarationName> {
        if stub.kind != DeclarationKind::Classifier
            || stub.flags.has(DeclarationFlags::LOCAL_CLASS)
            || stub.flags.has(DeclarationFlags::ANONYMOUS_OBJECT)
        {
            return None;
        }
        let name = match self
            .syntax
            .declaration(stub.id)
            .map(|declaration| declaration.kind)
        {
            Some(HeaderDeclarationKind::Classifier { source_name, .. }) => {
                self.lookup_names.get(source_name)?
            }
            None if stub.flags.has(DeclarationFlags::COMPILER_GENERATED) => {
                self.lookup_names.get(stub.lookup_name?)?
            }
            _ => return None,
        };
        match self.declarations.anchor(stub.id)?.owner {
            None => Some(ClassifierDeclarationName::TopLevel {
                package: self.sources.get(stub.source)?.package,
                name: name.into(),
            }),
            Some(owner) => {
                let owner = self.classifier_declaration_name(self.stub(owner)?)?;
                Some(ClassifierDeclarationName::Nested {
                    owner: identity(&owner),
                    name: name.into(),
                })
            }
        }
    }

    /// Qualified, JVM-neutral identities of non-local Kotlin classifiers published by this
    /// source module. Platform source providers use this once during Pass 1 to resolve foreign
    /// declaration headers against Kotlin declarations from the same compilation. Local and
    /// anonymous classifiers are deliberately absent: another source file cannot name them.
    pub fn source_classifier_names(&self) -> Vec<crate::types::TypeName> {
        self.source_classifiers()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// [`Self::source_classifier_names`] with each classifier's declared visibility, which a
    /// foreign-language provider needs to decide which nested classifiers its classes inherit.
    pub fn source_classifiers(&self) -> Vec<(crate::types::TypeName, crate::types::Visibility)> {
        let mut names = self
            .stubs
            .iter()
            .filter(|stub| stub.kind == DeclarationKind::Classifier)
            .filter(|stub| {
                !stub.flags.has(DeclarationFlags::LOCAL_CLASS)
                    && !stub.flags.has(DeclarationFlags::ANONYMOUS_OBJECT)
            })
            .filter_map(|stub| {
                let declaration = self.classifier_declaration_name(stub)?;
                Some((identity(&declaration), stub.visibility))
            })
            .collect::<Vec<_>>();
        let mut seen = crate::fx_hash::HashSet::default();
        names.retain(|(name, _)| seen.insert(*name));
        names
    }
}

fn identity(name: &ClassifierDeclarationName) -> TypeName {
    match name {
        ClassifierDeclarationName::TopLevel { package, name } => {
            crate::types::type_name_child(*package, name)
        }
        ClassifierDeclarationName::Nested { owner, name } => {
            crate::types::type_name_nested_child(*owner, name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{existing_type_name_child, type_name_child, Visibility};

    fn inventory(source: &str) -> StreamedHeaderModule {
        let mut diagnostics = crate::diag::DiagSink::new();
        let module = crate::fir::stream_file_stub_inventory(
            &[crate::frontend::SourceInput::kotlin(source)],
            &crate::features::LangFeatures::new(),
            &mut diagnostics,
            |_, _, _| {},
        );
        assert_eq!(diagnostics.diags.len(), 0, "{:?}", diagnostics.diags);
        module
    }

    #[test]
    fn source_classifier_publication_does_not_intern_undeclared_prefixes() {
        let module = inventory("package headerownedliteral\nclass `Only$Literal`\n");
        let package = module
            .sources
            .get(crate::fir::SourceFileId::from_raw(0))
            .unwrap()
            .package;
        assert_eq!(existing_type_name_child(package, "Only"), None);
        let published = module.source_classifiers();
        assert_eq!(
            published,
            [(type_name_child(package, "Only$Literal"), Visibility::Public)]
        );
        assert_eq!(existing_type_name_child(package, "Only"), None);
    }

    #[test]
    fn source_classifier_publication_preserves_declared_nested_segments() {
        let module =
            inventory("package headerownednested\nclass `Outer$Literal` { class `Leaf$Name` }\n");
        let package = module
            .sources
            .get(crate::fir::SourceFileId::from_raw(0))
            .unwrap()
            .package;
        let published = module.source_classifiers();
        let owner = type_name_child(package, "Outer$Literal");
        let member = type_name_child(package, "Outer$Literal$Leaf$Name");
        assert_eq!(
            published,
            [(owner, Visibility::Public), (member, Visibility::Public)]
        );
        assert_eq!(member.nested_owner(), Some(owner));
        assert_eq!(member.nested_segment_within(owner), Some("Leaf$Name"));
        assert_eq!(existing_type_name_child(package, "Outer"), None);
    }
}
