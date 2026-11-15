//! The Kotlin name (`KCallable.name`) of the declaration a callable reference selected.

use crate::ast::{File, Stmt};

use super::super::{
    CallableReferenceBinding, CallableReferenceTarget, ConstructorReferenceOuter, ExprLowering,
};

/// What a reference literal's `name` reads: the selected declaration's Kotlin name, and how its
/// receiver is bound.
pub(in crate::resolve) struct ReferenceName<'a> {
    pub(in crate::resolve) name: &'a str,
    pub(in crate::resolve) receiver: ReferenceReceiver,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::resolve) enum ReferenceReceiver {
    /// No value receiver: a top-level or local declaration, a classifier's callable, an unbound
    /// member, or a singleton receiver.
    None,
    /// A value receiver: the expression written before `::`, if the reference has one.
    Value,
    /// An implicit `this`.
    Implicit,
}

/// The name kotlinc gives the declaration `lowering` selected: a constructor is `<init>`, a
/// fun-interface constructor is the interface's own name, and `invoke` of a function value is
/// `invoke`. Every other declaration has its declared name.
pub(in crate::resolve) fn selected_reference_name<'a>(
    file: &'a File,
    lowering: &'a ExprLowering,
) -> Option<ReferenceName<'a>> {
    let (name, receiver) = match lowering {
        ExprLowering::LocalFunction {
            stmt_id,
            bound_receiver,
        }
        | ExprLowering::AdaptedLocalFunctionRef {
            stmt_id,
            bound_receiver,
            ..
        } => {
            let Stmt::LocalFun(function) = file.stmt(*stmt_id) else {
                return None;
            };
            (function.name.as_str(), explicit_when(*bound_receiver))
        }
        ExprLowering::SamConstructorReference { sam, .. } => {
            (sam.internal.nested_segment_ref(), ReferenceReceiver::None)
        }
        ExprLowering::ConstructorRef { outer, .. } => (
            "<init>",
            match outer {
                ConstructorReferenceOuter::Unbound => ReferenceReceiver::None,
                ConstructorReferenceOuter::Expression(_) => ReferenceReceiver::Value,
                ConstructorReferenceOuter::Implicit(_) => ReferenceReceiver::Implicit,
            },
        ),
        ExprLowering::TopLevelFunctionRef(reference) => {
            (reference.target.name.as_str(), ReferenceReceiver::None)
        }
        ExprLowering::AdaptedRef { target, .. } => (target.name.as_str(), ReferenceReceiver::None),
        ExprLowering::CallableReference { binding, target }
        | ExprLowering::AdaptedCallableReference {
            binding, target, ..
        } => (
            target_name(target),
            match binding {
                CallableReferenceBinding::Bound => ReferenceReceiver::Value,
                CallableReferenceBinding::ImplicitThis => ReferenceReceiver::Implicit,
                CallableReferenceBinding::Unbound | CallableReferenceBinding::Singleton(_) => {
                    ReferenceReceiver::None
                }
            },
        ),
        ExprLowering::FunctionInvokeReference { .. } => ("invoke", ReferenceReceiver::Value),
        _ => return None,
    };
    Some(ReferenceName { name, receiver })
}

fn explicit_when(bound: bool) -> ReferenceReceiver {
    if bound {
        ReferenceReceiver::Value
    } else {
        ReferenceReceiver::None
    }
}

fn target_name(target: &CallableReferenceTarget) -> &str {
    match target {
        CallableReferenceTarget::Classifier(member)
        | CallableReferenceTarget::Member { member, .. } => &member.name,
        CallableReferenceTarget::ClassifierProperty { name, .. } => name,
        CallableReferenceTarget::Extension { callable, .. } => &callable.name,
        CallableReferenceTarget::Property(property) => &property.name,
        CallableReferenceTarget::TopLevelProperty(property) => &property.name,
    }
}
