//! The non-local Kotlin classifiers a source module publishes to foreign-language providers.

use super::{DeclarationFlags, DeclarationKind, StreamedHeaderModule};

impl StreamedHeaderModule {
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
                let spelling = self.lookup_names.get(stub.lookup_name?)?;
                let mut segments = spelling
                    .split(['.', '$'])
                    .filter(|segment| !segment.is_empty());
                let first = segments.next()?;
                let package = self.sources.get(stub.source)?.package;
                let mut identity = crate::types::type_name_child(package, first);
                for segment in segments {
                    identity = crate::types::type_name_nested_child(identity, segment);
                }
                Some((identity, stub.visibility))
            })
            .collect::<Vec<_>>();
        let mut seen = crate::fx_hash::HashSet::default();
        names.retain(|(name, _)| seen.insert(*name));
        names
    }
}
