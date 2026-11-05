//! Preserve the selected enclosing instance while its value crosses capture storage boundaries.

use super::*;

pub(super) struct CheckedClassCaptureSource {
    pub(super) source: FirLocalClassCaptureSource,
    pub(super) instance_owner: Option<crate::fir::EnclosingInstanceOwner>,
}

fn checked_instance_owner(
    index: &ResolvedModuleIndex,
    classifier: crate::types::TypeName,
    prior: Option<crate::fir::EnclosingInstanceOwner>,
    selected: crate::fir::EnclosingInstanceOwner,
) -> Option<crate::fir::EnclosingInstanceOwner> {
    if prior.is_some_and(|prior| prior != selected)
        || !matches!(
            (
                selected.kind,
                index
                    .declaration_anchor(selected.declaration)
                    .map(|anchor| anchor.kind)
            ),
            (
                crate::fir::EnclosingInstanceKind::Classifier,
                Some(DeclarationKind::Classifier)
            ) | (
                crate::fir::EnclosingInstanceKind::EnumEntry,
                Some(DeclarationKind::EnumEntry)
            )
        )
        || index
            .enclosing_classifier(selected.declaration)
            .map(|class| class.classifier)
            != Some(classifier)
    {
        return None;
    }
    Some(selected)
}

impl BodyFirChecker<'_> {
    /// Different lexical views of one field must agree about the instance it holds. Receiver
    /// bindings and ordinary value bindings are parts of the same declaration-owned inventory.
    pub(super) fn selected_storage_capture_binding(
        &self,
        owner: DeclarationId,
        field: u32,
        span: Span,
    ) -> Result<Option<ClassCaptureBinding>, BodyCheckFailure> {
        let mut selected: Option<ClassCaptureBinding> = None;
        for binding in self
            .class_capture_values
            .iter()
            .map(|(_, binding)| *binding)
            .chain(self.class_receivers.iter().copied())
            .filter(|binding| binding.owner == owner && binding.field == field)
        {
            if selected.is_some_and(|previous| {
                previous.enclosing_instance_owner != binding.enclosing_instance_owner
                    || previous.capture_identity != binding.capture_identity
                    || previous.ty != binding.ty
                    || previous.shared_cell != binding.shared_cell
            }) {
                return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
            }
            selected.get_or_insert(binding);
        }
        Ok(selected)
    }

    pub(super) fn checked_capture_receiver(
        &self,
        receiver: Option<&FirCapturedReceiver>,
        instance_owner: Option<crate::fir::EnclosingInstanceOwner>,
        span: Span,
    ) -> Result<Option<FirCapturedReceiver>, BodyCheckFailure> {
        let Some(FirCapturedReceiver::Enclosing { classifier, owner }) = receiver else {
            return Ok(receiver.cloned());
        };
        let Some(selected) = instance_owner else {
            return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
        };
        if checked_instance_owner(self.index, *classifier, *owner, selected).is_none() {
            return Err(self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
        }
        Ok(Some(FirCapturedReceiver::Enclosing {
            classifier: *classifier,
            owner: Some(selected),
        }))
    }

    /// Read the original declaring instance at the endpoint of an already-selected path.
    pub(super) fn path_instance_owner(
        &self,
        path: &[DeclarationId],
        span: Option<Span>,
    ) -> Result<crate::fir::EnclosingInstanceOwner, BodyCheckFailure> {
        let owner = match path.last() {
            Some(last) => self
                .index
                .declaration_anchor(*last)
                .and_then(|anchor| anchor.owner),
            None => self.current_storage_owner(),
        }
        .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))?;
        self.enclosing_instance(owner)
            .enclosing_owner()
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::MissingStableCallTarget))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_enum_type_does_not_alias_selected_entry_owners() {
        let (_, index) = super::super::test_support::checked_function_body(
            "enum class Choice { X, Y }\nfun token(): Int = 0\n",
            "token",
        );
        let classifier = crate::types::type_name("Choice");
        let enum_class = index
            .classifier_declaration(classifier)
            .expect("enum declaration");
        let entries = index
            .owned_declarations(enum_class)
            .iter()
            .copied()
            .filter(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.kind == DeclarationKind::EnumEntry)
            })
            .collect::<Vec<_>>();
        let [x, y] = entries.as_slice() else {
            panic!("two declared enum entries")
        };
        let owner = |declaration| crate::fir::EnclosingInstanceOwner {
            declaration,
            kind: crate::fir::EnclosingInstanceKind::EnumEntry,
        };
        assert_eq!(
            checked_instance_owner(&index, classifier, None, owner(*x)),
            Some(owner(*x))
        );
        assert_eq!(
            checked_instance_owner(&index, classifier, Some(owner(*x)), owner(*x)),
            Some(owner(*x))
        );
        assert_eq!(
            checked_instance_owner(&index, classifier, Some(owner(*x)), owner(*y)),
            None
        );
        assert_eq!(
            checked_instance_owner(
                &index,
                Ty::String.obj_internal().expect("String classifier"),
                None,
                owner(*x)
            ),
            None
        );
        assert_eq!(
            checked_instance_owner(
                &index,
                classifier,
                None,
                crate::fir::EnclosingInstanceOwner {
                    declaration: *x,
                    kind: crate::fir::EnclosingInstanceKind::Classifier,
                }
            ),
            None
        );
    }
}
