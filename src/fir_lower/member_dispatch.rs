//! The classifier a member call's dispatch receiver statically has.

use super::BodyLowering;
use crate::fir::{FirConversionKind, FirReceiver};
use crate::ir::ExprId;
use crate::types::{Ty, TypeName};

impl BodyLowering<'_> {
    /// The class type `receiver` statically has, after any smart cast: the class the checker
    /// selected the member through. A receiver of any other type (a type parameter) gives none.
    pub(super) fn dispatch_class(&self, receiver: Option<FirReceiver>) -> Option<TypeName> {
        let receiver = receiver?;
        let ty = match receiver.conversion.map(|conversion| conversion.kind) {
            Some(FirConversionKind::SmartCast { to }) => to.get(),
            _ => self.body.expr(receiver.value)?.ty.get(),
        };
        match ty.non_null() {
            Ty::Obj(class, _) => Some(class),
            _ => None,
        }
    }

    /// The class a resolved receiver type names, when it is a classifier.
    pub(super) fn static_class(ty: Option<crate::fir::ResolvedTy>) -> Option<TypeName> {
        ty.and_then(|ty| ty.get().non_null().obj_internal())
    }

    pub(super) fn record_dispatch_class(&mut self, call: ExprId, class: Option<TypeName>) {
        if let Some(class) = class {
            self.ir.dispatch_classes.insert(call, class);
        }
    }
}
