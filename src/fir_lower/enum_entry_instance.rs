//! A reference to the enum entry whose body is executing.
//!
//! The static field that publishes an entry is assigned only after that entry's constructor
//! returns. Code that belongs to the entry — its own members, and a class nested in it — therefore
//! reads the entry instance. The expression's type is the enum, so the instance is cast to it. A
//! reference to any other entry stays a static-field read.

use crate::fir::DeclarationId;
use crate::ir::{ExprId, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// The current entry instance when `classifier.name` is the enum entry this body belongs to.
    ///
    /// `None` leaves the ordinary static-field read in place: the body is not inside that entry,
    /// or it has no dispatch receiver from which the instance can be reached.
    pub(super) fn same_enum_entry_instance(
        &mut self,
        classifier: crate::types::TypeName,
        name: &str,
        origin: crate::fir::OriginId,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        let Some(path) = self.enclosing_same_enum_entry_path(classifier, name) else {
            return Ok(None);
        };
        let instance = if path.is_empty() {
            let Some(slot) = self.dispatch_receiver_slot() else {
                return Ok(None);
            };
            self.ir.add_expr(IrExpr::GetValue(slot))
        } else {
            self.enclosing_receiver(&path, origin)?
        };
        Ok(Some(self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: instance,
            type_operand: Ty::obj_name(classifier),
        })))
    }

    /// Classifiers between this body and its enclosing enum entry, innermost first.
    ///
    /// An empty path means the body is the entry class itself (`this`). `None` means this body is
    /// not inside the named entry.
    fn enclosing_same_enum_entry_path(
        &self,
        classifier: crate::types::TypeName,
        name: &str,
    ) -> Option<Vec<DeclarationId>> {
        use crate::fir::DeclarationKind;
        let mut current = DeclarationId::from_raw(self.body.owner().raw());
        let mut path = Vec::new();
        loop {
            let anchor = self.index.declaration_anchor(current)?;
            if anchor.kind == DeclarationKind::EnumEntry {
                let entry_name = self.index.declaration_name(current)?;
                if entry_name != name {
                    return None;
                }
                let enum_declaration = anchor.owner?;
                let header = self.index.classifier_header(enum_declaration)?;
                if header.classifier != classifier {
                    return None;
                }
                return Some(path);
            }
            if anchor.kind == DeclarationKind::Classifier {
                path.push(current);
            }
            current = anchor.owner?;
        }
    }
}
