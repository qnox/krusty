//! Record each checked call of a protected member with the receiver it was selected through.

use crate::fir::{DeclarationId, ResolvedModuleIndex, ResolvedParameterIdentity};
use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrFile, IrProtectedMemberCall};
use crate::types::{Ty, Visibility};

/// One checked member call: the member's declaration, the visibility of what it accesses, the
/// checked type of its dispatch receiver and the member's parameters.
pub(super) struct MemberCall {
    pub(super) call: ExprId,
    pub(super) declaration: DeclarationId,
    pub(super) visibility: Visibility,
    pub(super) receiver: Option<Ty>,
    pub(super) parameter_identities: Vec<ResolvedParameterIdentity>,
}

/// Record `call` when it accesses a protected member.
pub(super) fn record_protected_member_call(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    call: MemberCall,
) -> Option<()> {
    if call.visibility != Visibility::Protected {
        return Some(());
    }
    let declaring = index.enclosing_classifier(call.declaration)?;
    let receiver = call.receiver?.non_null().obj_internal()?;
    let declaring_type_parameters = (0..)
        .map_while(|ordinal| index.type_parameter(declaring.declaration, ordinal))
        .map(|parameter| {
            index
                .type_parameter_semantic_name(parameter)
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()?;
    ir.protected_member_calls.insert(
        call.call,
        IrProtectedMemberCall {
            declaring: declaring.classifier,
            receiver,
            declaring_type_parameters,
            parameter_identities: call.parameter_identities,
        },
    );
    Some(())
}

/// How an expression uses a sibling-file property's accessors.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PropertyUse {
    Read,
    Write,
    /// A reference, which reads the property and, when mutable, writes it.
    Reference {
        mutable: bool,
    },
}

/// Record the uses of protected sibling-file property accessors: a read of a protected property,
/// a write through a protected setter, and a reference to either.
pub(super) fn record_protected_property_calls(index: &ResolvedModuleIndex, ir: &mut IrFile) {
    let uses = (0..ir.exprs.len() as ExprId)
        .filter_map(|call| match ir.expr(call) {
            IrExpr::Checked(IrCheckedOperation::PropertyRead {
                target,
                dispatch_receiver: Some(receiver),
                ..
            }) => Some((
                call,
                *target,
                ir.logical_types.get(receiver).copied(),
                PropertyUse::Read,
            )),
            IrExpr::Checked(IrCheckedOperation::PropertyWrite {
                target,
                dispatch_receiver: Some(receiver),
                ..
            }) => Some((
                call,
                *target,
                ir.logical_types.get(receiver).copied(),
                PropertyUse::Write,
            )),
            IrExpr::Checked(IrCheckedOperation::PropertyReference {
                target:
                    crate::fir::FirPropertyReferenceTarget::SpecializedModule {
                        property,
                        receiver: Some(receiver),
                        extension_receiver: false,
                        ..
                    },
                delegated: false,
                mutable,
                ..
            }) => Some((
                call,
                *property,
                Some(receiver.get()),
                PropertyUse::Reference { mutable: *mutable },
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (call, target, receiver, usage) in uses {
        let (Some(property), Some(module)) = (
            index.property(target),
            ir.referenced_module_properties.get(&target),
        ) else {
            continue;
        };
        let setter = index.property_setter_parameter_identity(target).cloned();
        let (visibility, parameter_identities) = match usage {
            PropertyUse::Read => (module.visibility, Vec::new()),
            PropertyUse::Write => (module.setter_visibility, setter.into_iter().collect()),
            PropertyUse::Reference { mutable } => {
                let protected = module.visibility == Visibility::Protected
                    || mutable && module.setter_visibility == Visibility::Protected;
                let visibility = if protected {
                    Visibility::Protected
                } else {
                    module.visibility
                };
                (visibility, setter.filter(|_| mutable).into_iter().collect())
            }
        };
        let call = MemberCall {
            call,
            declaration: property.declaration,
            visibility,
            receiver,
            parameter_identities,
        };
        record_protected_member_call(index, ir, call);
    }
}
