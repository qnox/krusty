//! Checked `inner`-classifier facts used while metadata splits a semantic type application.
//!
//! A semantic application keeps an inner classifier's own type arguments first and the captured
//! outer classes' arguments after them. Metadata writes the latter on `Type.outer_type`, so the
//! serializer needs the declaration boundary rather than recovering one from a flattened name.

use crate::backend::BackendClassifierSource;
use crate::ir::IrFile;
use crate::metadata::InnerClassifier;
use crate::types::TypeName;

/// The split of `classifier` when it is an `inner` class. `Ok(None)` means a checked non-inner
/// declaration; `Err(classifier)` means the backend boundary supplied no declaration fact.
pub(crate) fn inner_classifier(
    ir: &IrFile,
    symbols: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Result<Option<InnerClassifier>, TypeName> {
    // A top-level classifier cannot capture an outer instance, so its declaration is irrelevant to
    // the metadata split. This also keeps backend-only/unit fixtures honest: they need facts only
    // for nested classifiers, where choosing the flattened shape would be an actual fallback.
    if classifier.nested_owner().is_none() {
        return Ok(None);
    }
    if let Some(class) = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name == classifier)
    {
        return if class.is_inner_class {
            classifier
                .nested_owner()
                .map(|outer| {
                    Some(InnerClassifier {
                        outer,
                        own_arguments: class.type_params.len(),
                    })
                })
                .ok_or(classifier)
        } else {
            Ok(None)
        };
    }
    let fact = symbols.classifier(classifier).ok_or(classifier)?;
    Ok(fact.outer_instance.map(|outer| InnerClassifier {
        outer,
        own_arguments: fact.own_type_parameter_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoClassifierFacts;

    impl BackendClassifierSource for NoClassifierFacts {
        fn classifier(
            &self,
            _: TypeName,
        ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
            None
        }
    }

    #[test]
    fn a_top_level_classifier_needs_no_inner_class_fact() {
        let ir = IrFile::default();
        let classifier = crate::types::type_name("kotlin/coroutines/Continuation");

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Ok(None)
        );
    }

    #[test]
    fn a_nested_classifier_still_requires_a_checked_fact() {
        let ir = IrFile::default();
        let classifier = crate::types::type_name("sample/Outer").nested_child("Nested");

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Err(classifier)
        );
    }
}
