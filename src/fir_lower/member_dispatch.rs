//! Declared-owner applications and static classes of selected dispatch receivers.

use super::BodyLowering;
use crate::fir::{FirConversionKind, FirReceiver};
use crate::ir::ExprId;
use crate::types::TyKind;
use crate::types::TypeName;

impl BodyLowering<'_> {
    /// Apply the runtime subclass authenticated by a stable enum-entry declaration.
    pub(super) fn apply_enum_entry_receiver(
        &mut self,
        owner: crate::fir::DeclarationId,
        receiver: ExprId,
    ) -> Result<(ExprId, TypeName), super::FirLoweringFailure> {
        let class = self
            .ir
            .checked_enum_entry_classes
            .get(&owner)
            .copied()
            .ok_or(super::FirLoweringFailure::MissingLocalClass(owner))?;
        let classifier = self.ir.classes[class as usize].fq_name_id();
        let receiver = self.ir.add_expr(crate::ir::IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: receiver,
            type_operand: crate::types::Ty::obj_name(classifier),
        });
        Ok((receiver, classifier))
    }

    /// A selected module property already owns its declaration. An entry-owned property requires
    /// that exact entry's receiver application before either an accessor or backing-field use.
    pub(super) fn module_property_receiver(
        &mut self,
        property: crate::fir::PropertyId,
        receiver: Option<FirReceiver>,
    ) -> Result<(Option<ExprId>, Option<TypeName>), super::FirLoweringFailure> {
        let declaration = self
            .index
            .property(property)
            .ok_or(super::FirLoweringFailure::MissingProperty(property))?
            .declaration;
        let anchor = self
            .index
            .declaration_anchor(declaration)
            .ok_or(super::FirLoweringFailure::MissingProperty(property))?;
        let owner = match anchor.owner {
            Some(owner) => {
                let owner_anchor = self
                    .index
                    .declaration_anchor(owner)
                    .ok_or(super::FirLoweringFailure::MissingProperty(property))?;
                (owner_anchor.kind == crate::fir::DeclarationKind::EnumEntry).then_some(owner)
            }
            None => None,
        };
        let dispatch_class = self.dispatch_class(receiver);
        let receiver = self.receiver(receiver)?;
        match owner {
            Some(owner) => {
                let receiver =
                    receiver.ok_or(super::FirLoweringFailure::MissingLocalClass(owner))?;
                let (receiver, classifier) = self.apply_enum_entry_receiver(owner, receiver)?;
                Ok((Some(receiver), Some(classifier)))
            }
            None => Ok((receiver, dispatch_class)),
        }
    }

    /// The class type `receiver` statically has, after any smart cast: the class the checker
    /// selected the member through. A receiver of any other type (a type parameter) gives none.
    pub(super) fn dispatch_class(&self, receiver: Option<FirReceiver>) -> Option<TypeName> {
        let receiver = receiver?;
        let ty = match receiver.conversion.map(|conversion| conversion.kind) {
            Some(FirConversionKind::SmartCast { to }) => to.get(),
            _ => self.body.expr(receiver.value)?.ty.get(),
        };
        match ty.non_null().kind() {
            TyKind::Obj(class, _) => Some(class),
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
