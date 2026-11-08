//! Checked classifier facts used while metadata splits a semantic type application.
//!
//! A semantic application keeps a classifier's own type arguments first. An `inner` class then
//! carries each enclosing instance's arguments, which metadata writes on `Type.outer_type`. A
//! local or anonymous classifier carries lexical captures in that same suffix, and metadata
//! records none of them: the serializer needs the declaration's own/captured layout rather than
//! recovering one from a flattened name.

use crate::backend::BackendClassifierSource;
use crate::ir::{IrClass, IrFile};
use crate::metadata::InnerClassifier;
use crate::types::TypeName;

/// The metadata split of `classifier`. `Ok(None)` means every semantic argument is its own.
/// `Err(classifier)` means the backend boundary supplied no declaration fact.
pub(crate) fn inner_classifier(
    ir: &IrFile,
    symbols: &dyn BackendClassifierSource,
    classifier: TypeName,
) -> Result<Option<InnerClassifier>, TypeName> {
    // Current-file classifiers, including a local one whose JVM name has no classifier owner,
    // publish their own/captured layout on the IR class. A top-level name that is not in this
    // file cannot capture an outer instance, so it needs no fact; a nested one still does.
    if let Some(class) = ir
        .classes
        .iter()
        .find(|candidate| candidate.fq_name == classifier)
    {
        return application(ir, class);
    }
    if classifier.nested_owner().is_none() {
        return Ok(None);
    }
    let fact = symbols.classifier(classifier).ok_or(classifier)?;
    Ok(fact.outer_instance.map(|outer| InnerClassifier {
        outer: Some(outer),
        own_arguments: fact.own_type_parameter_count,
    }))
}

fn application(ir: &IrFile, class: &IrClass) -> Result<Option<InnerClassifier>, TypeName> {
    if class.is_inner_class {
        let outer = class.fq_name.nested_owner().ok_or(class.fq_name)?;
        return Ok(Some(InnerClassifier {
            outer: Some(outer),
            own_arguments: class.type_params.len(),
        }));
    }
    if !class.captured_type_params.is_empty()
        && crate::metadata::local_classifiers::is_local(ir, class)
    {
        return Ok(Some(InnerClassifier {
            outer: None,
            own_arguments: class.type_params.len(),
        }));
    }
    Ok(None)
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

    fn local_class(name: &str, own: &[&str], captured: &[&str], anonymous: bool) -> IrFile {
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(crate::types::type_name(name));
        class.is_anonymous_object = anonymous;
        class.is_local_class = !anonymous;
        class.type_params = own.iter().map(|name| (*name).to_owned()).collect();
        class.captured_type_params = captured.iter().map(|name| (*name).to_owned()).collect();
        ir.classes.push(class);
        ir
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

    #[test]
    fn an_anonymous_factory_owns_no_arguments_and_has_no_outer_type() {
        let ir = local_class("app/FileKt$copy$1", &[], &["T"], true);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Ok(Some(InnerClassifier {
                outer: None,
                own_arguments: 0,
            }))
        );
    }

    #[test]
    fn a_named_local_class_keeps_its_own_arguments_apart_from_enclosing_captures() {
        let ir = local_class("app/FileKt$enclose$Holder", &["U"], &["T"], false);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Ok(Some(InnerClassifier {
                outer: None,
                own_arguments: 1,
            }))
        );
    }

    #[test]
    fn a_local_class_without_captures_uses_every_argument() {
        let ir = local_class("app/FileKt$enclose$Holder", &["U"], &[], false);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Ok(None)
        );
    }

    #[test]
    fn an_inner_class_still_names_its_enclosing_instance() {
        let outer = crate::types::type_name("sample/Outer");
        let mut ir = IrFile::default();
        let mut class = IrClass::synthetic(outer.nested_child("Inner"));
        class.is_inner_class = true;
        class.type_params = vec!["U".to_owned()];
        class.captured_type_params = vec!["T".to_owned()];
        ir.classes.push(class);
        let classifier = ir.classes[0].fq_name;

        assert_eq!(
            inner_classifier(&ir, &NoClassifierFacts, classifier),
            Ok(Some(InnerClassifier {
                outer: Some(outer),
                own_arguments: 1,
            }))
        );
    }
}
