//! Bind-once declaration provenance of an annotation type occurrence.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BoundAnnotation {
    pub(super) classifier: TypeName,
    alias: Option<TypeName>,
    classifier_segment: Option<u32>,
}

pub(super) type BoundAnnotations = HashMap<(u32, u32, u32), BoundAnnotation>;

impl ClassNames {
    pub(super) fn annotation_binding(&self, spelling: &str) -> Option<BoundAnnotation> {
        Some(BoundAnnotation {
            classifier: self.get_class(spelling)?,
            alias: self.alias_identity(spelling),
            classifier_segment: self.classifier_segment(spelling),
        })
    }
}

impl Checker<'_> {
    pub(super) fn bound_annotation_owner(
        &mut self,
        scope: &CheckerScope<'_>,
        binding: BoundAnnotation,
        reference: &TypeRef,
    ) -> Ty {
        let stripped = if reference.qualifier_arguments.is_empty() {
            None
        } else {
            self.check_selected_package_type_arguments(scope, reference, binding.classifier_segment)
        };
        let reference = stripped.as_ref().unwrap_or(reference);
        if let Some(alias) = binding.alias {
            let applied = self.source_alias_application_ty(
                scope,
                alias,
                &reference.name,
                alias_projection::AliasArguments::of(reference),
                reference.span,
            );
            self.report_alias_projection_conflicts(&applied.conflicts, applied.ty);
            applied.ty
        } else {
            self.obj_with_targs_name(scope, binding.classifier, reference)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_classifier_override_does_not_keep_a_lower_alias_with_the_same_target() {
        let target = type_name("Host$A");
        let alias = type_name("other/Alias");
        let mut names = ClassNames::default();
        names.insert_name("A".to_owned(), target);
        names.insert_alias_expansion(
            "A".to_owned(),
            crate::libraries::AliasExpansion {
                identity: alias,
                declaration_name: None,
                target,
                formals: vec!["T".to_owned()],
                expansion: Ty::obj_args_name(target, &[Ty::obj("kotlin/String")]),
                expansion_spelling: Default::default(),
            },
        );
        assert_eq!(
            names.annotation_binding("A"),
            Some(BoundAnnotation {
                classifier: target,
                alias: Some(alias),
                classifier_segment: None,
            })
        );
        names.insert_name("A".to_owned(), target);
        assert_eq!(
            names.annotation_binding("A"),
            Some(BoundAnnotation {
                classifier: target,
                alias: None,
                classifier_segment: None,
            })
        );
    }
}
