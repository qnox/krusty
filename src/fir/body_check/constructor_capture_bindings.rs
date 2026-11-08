//! Constructor-prefix bindings for signature-owned defaults, before ordinary body dispatch.

use super::*;
use crate::resolve::{AnonymousObjectCapture, AnonymousObjectCaptureSource};

/// Install the existing hidden constructor-prefix arities from the current checked source.
pub(super) fn bind_constructor_prefix_shape(
    body: &mut FirBody,
    file: &crate::ast::File,
    info: &TypeInfo,
    index: &ResolvedModuleIndex,
    session: &BodyCheckSession,
    declaration: DeclarationId,
    enclosing_classifier: Option<&crate::fir::ResolvedClassifierHeader>,
) {
    let constructor_capture_parameter_count = index
        .declaration_anchor(declaration)
        .filter(|anchor| {
            matches!(
                anchor.kind,
                crate::fir::DeclarationKind::Constructor
                    | crate::fir::DeclarationKind::Initializer
                    | crate::fir::DeclarationKind::Property
            )
        })
        .and(enclosing_classifier)
        .map(|classifier| {
            let transient = session
                .active_source
                .as_ref()
                .and_then(|active| {
                    active
                        .class(file, classifier.declaration)
                        .map(|(declaration, _)| declaration)
                })
                .or_else(|| {
                    let range = index.declaration_range(classifier.declaration)?;
                    file.decl_arena.iter().enumerate().find_map(|(raw, declaration)| {
                        if matches!(declaration, crate::ast::Decl::Class(class) if class.span == range)
                        {
                            Some(crate::ast::DeclId(u32::try_from(raw).ok()?))
                        } else {
                            None
                        }
                    })
                });
            let captures = transient
                .and_then(|declaration| {
                    info.local_class_captures_by_class
                        .get(&declaration)
                        .or_else(|| info.anonymous_object_captures_by_class.get(&declaration))
                })
                .map_or(0, Vec::len);
            let implicit_outer = index
                .declaration_header(classifier.declaration)
                .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::INNER));
            u32::try_from(captures)
                .expect("too many checked classifier captures")
                .checked_add(u32::from(implicit_outer))
                .expect("too many constructor prefix parameters")
        })
        .unwrap_or(0);
    if constructor_capture_parameter_count != 0 {
        body.set_constructor_capture_parameter_count(constructor_capture_parameter_count);
    }
    let constructor_context_parameter_count = index
        .declaration_anchor(declaration)
        .filter(|anchor| {
            matches!(
                anchor.kind,
                crate::fir::DeclarationKind::Constructor
                    | crate::fir::DeclarationKind::Initializer
                    | crate::fir::DeclarationKind::Property
            )
        })
        .and(enclosing_classifier)
        // A script's implicit receivers follow its template's constructor parameters instead.
        .filter(|classifier| {
            !index
                .declaration_header(classifier.declaration)
                .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::SCRIPT))
        })
        .and_then(|classifier| index.classifier_header(classifier.declaration))
        .map(|classifier| {
            u32::try_from(classifier.context_parameters.len())
                .expect("too many classifier context parameters")
        })
        .unwrap_or(0);
    if constructor_context_parameter_count != 0 {
        body.set_constructor_context_parameter_count(constructor_context_parameter_count);
    }
}

impl BodyFirChecker<'_> {
    pub(super) fn bind_signature_constructor_capture_prefix(
        &mut self,
        active: &ActiveSourceDeclarations,
        constructor: DeclarationId,
        span: Span,
    ) -> Result<(), BodyCheckFailure> {
        let classifier = self
            .index
            .enclosing_classifier(constructor)
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        if !self
            .index
            .owns_local_capture_environment(classifier.declaration)
        {
            return Ok(());
        }
        let (transient, _) = active
            .class(self.file, classifier.declaration)
            .ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
        let inventory = if self
            .file
            .anonymous_object_classes
            .values()
            .any(|declaration| *declaration == transient)
        {
            self.info.anonymous_object_captures_by_class.get(&transient)
        } else {
            self.info.local_class_captures_by_class.get(&transient)
        };
        // Absence does not establish an empty layout. Leave the environment unavailable;
        // a captured read still requires its exact selected binding through the ordinary checker.
        let Some(captures) = inventory else {
            return Ok(());
        };
        let mut context = ClassBodyContext::default();
        let mut identities = HashMap::<ClassCaptureIdentity, ClassCaptureBinding>::default();
        for (field, capture) in captures.iter().enumerate() {
            let identity = capture
                .logical_identity(classifier.classifier)
                .ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })?;
            let field = u32::try_from(field).map_err(|_| {
                self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
            })?;
            let receiver_depth = capture
                .semantic_receiver
                .map(crate::resolve::AnonymousObjectReceiverSource::depth)
                .or(match capture.source {
                    AnonymousObjectCaptureSource::EnclosingInstance { depth, .. }
                    | AnonymousObjectCaptureSource::ImplicitReceiver { depth, .. } => Some(depth),
                    _ => None,
                })
                .map(|depth| {
                    depth.checked_add(1).ok_or_else(|| {
                        self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                    })
                })
                .transpose()?;
            let binding = ClassCaptureBinding {
                owner: classifier.declaration,
                field,
                ty: self.resolved_type(span, capture.storage_ty.unwrap_or(capture.ty))?,
                shared_cell: capture.storage_ty.is_none() && capture.shared_cell,
                enclosing_depth: 0,
                semantic_receiver_depth: receiver_depth,
                receiver_source: None,
                enclosing_instance_owner: self.signature_capture_instance_owner(
                    capture,
                    classifier.classifier,
                    span,
                )?,
                capture_identity: Some(identity),
                lexical_binding: capture.lexical_binding,
            };
            if receiver_depth.is_some() {
                context.receivers.push(binding);
            } else if capture.storage_ty.is_some() {
                // A delegate's operand is its stored object, not the property's getter value.
                // Existing checked convention bindings remain authoritative and are not invented here.
                context.capture_values.push((capture.name.clone(), binding));
            } else {
                context.record_value(capture.name.clone(), binding, capture.lexical_shadow_depth);
            }
            let views = std::iter::once(identity)
                .chain(capture.constructor_projections.iter().copied())
                .chain(capture.source_identity(classifier.classifier));
            for view in views {
                let projected = ClassCaptureBinding {
                    capture_identity: Some(view),
                    ..binding
                };
                if let Some(previous) = identities.insert(view, projected) {
                    if previous != projected {
                        return Err(
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        );
                    }
                } else if view != identity {
                    context
                        .capture_values
                        .push((capture.name.clone(), projected));
                }
            }
        }
        self.class_values
            .retain(|_, binding| binding.owner != classifier.declaration);
        self.class_values.extend(context.values);
        self.class_capture_values
            .retain(|(_, binding)| binding.owner != classifier.declaration);
        self.class_capture_values.extend(context.capture_values);
        self.class_receivers
            .retain(|binding| binding.owner != classifier.declaration);
        self.class_receivers.extend(context.receivers);
        Ok(())
    }

    fn signature_capture_instance_owner(
        &self,
        capture: &AnonymousObjectCapture,
        capture_owner: crate::types::TypeName,
        span: Span,
    ) -> Result<Option<crate::fir::EnclosingInstanceOwner>, BodyCheckFailure> {
        let Some(FirCapturedReceiver::Enclosing { owner, .. }) = capture.receiver.as_ref() else {
            return Ok(None);
        };
        let selected = match owner {
            Some(owner) => *owner,
            None => {
                let Some(ClassCaptureIdentity::EnclosingClass(classifier)) =
                    capture.source_identity(capture_owner)
                else {
                    return Err(
                        self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    );
                };
                let declaration =
                    self.index
                        .classifier_declaration(classifier)
                        .ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                self.enclosing_instance(declaration)
                    .enclosing_owner()
                    .ok_or_else(|| {
                        self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    })?
            }
        };
        self.checked_capture_receiver(capture.receiver.as_ref(), Some(selected), span)?;
        Ok(Some(selected))
    }
}
