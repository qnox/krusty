//! The lexical identity of a declared receiver: the label `this@label` names it by, and the
//! declaration span that owns it.

use super::*;

fn lexical_receiver_label(file: &File, reference: &TypeRef) -> String {
    file.alias_spellings
        .get(&reference.span)
        .unwrap_or(reference)
        .name
        .rsplit(['.', '/'])
        .next()
        .unwrap_or(&reference.name)
        .to_string()
}

pub(super) fn lexical_receiver_declaration(file: &File, reference: &TypeRef) -> (Span, String) {
    (reference.span, lexical_receiver_label(file, reference))
}

pub(super) fn lexical_context_receiver(file: &File, parameter: &Param, ty: Ty) -> ContextReceiver {
    let kind = match parameter.context_kind {
        crate::types::ContextParameterKind::Named => {
            return ContextReceiver::named(ty, parameter.name.clone())
                .with_declaration(parameter.ty.span)
        }
        crate::types::ContextParameterKind::Anonymous => ContextReceiverKind::Anonymous,
        crate::types::ContextParameterKind::LegacyReceiver => ContextReceiverKind::LegacyReceiver,
        crate::types::ContextParameterKind::None => unreachable!("a context parameter has a kind"),
    };
    ContextReceiver::new(ty, kind, Some(lexical_receiver_label(file, &parameter.ty)))
        .with_declaration(parameter.ty.span)
}
