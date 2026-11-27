//! Explicit imports of a singleton's members (`import pkg.Holder.member`): the singleton value
//! that receives the imported name, and the nested classifiers such an import does not name.

use super::*;

impl Checker<'_> {
    /// Explicit import of a member declared on a singleton classifier. The import scope has already
    /// resolved the owner path and alias to stable identities; this only supplies the ordinary value
    /// receiver used by member/property selection.
    pub(super) fn imported_singleton_member(
        &self,
        name: &str,
    ) -> Option<(ImplicitReceiver, String, SingletonValue)> {
        let mut candidates = self
            .function_import_scope
            .explicit_targets(name)
            .into_iter()
            .filter_map(|(namespace, declared_name)| {
                crate::trace_compiler!(
                    "resolve",
                    "imported singleton candidate name={name} target={namespace:?}.{declared_name}"
                );
                let crate::symbol_source::SymbolNamespace::Classifier(classifier) = namespace
                else {
                    return None;
                };
                let singleton = self.classifier_singleton_value(classifier)?;
                // The import names a member only when the object declares one under that name; a
                // nested classifier of the object is imported through the classifier namespace.
                crate::symbol_resolver::imported_object_member_symbols(
                    &self.fed_source(),
                    classifier,
                    &declared_name,
                )?;
                (singleton.classifier == classifier).then_some((
                    ImplicitReceiver {
                        receiver_role: None,
                        ty: Ty::obj_name(classifier),
                        declared_ty: Ty::obj_name(classifier),
                        identity: (0, 0),
                        extension_receiver: None,
                        class_receiver: false,
                        current: false,
                        receiver_depth: 0,
                        dsl_shadowed: false,
                    },
                    declared_name,
                    singleton,
                ))
            })
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return None;
        }
        let (receiver, declared_name, singleton) = candidates.pop()?;
        crate::trace_compiler!(
            "resolve",
            "imported singleton classifier={:?} storage={singleton:?}",
            singleton.classifier
        );
        Some((receiver, declared_name, singleton))
    }
}
