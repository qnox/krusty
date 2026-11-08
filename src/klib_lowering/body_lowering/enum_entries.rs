//! A read of a dependency enum entry.
//!
//! `GetEnumValue` names one entry of one enum class. The linker proves that entry from the
//! producer graph (or, for a template, from the selected library record). The common node is the
//! same `EnumEntry` checked FIR lowering records for a source read: the enum's classifier and the
//! entry's declaration name. A target realizes a constant another file declares.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// The value of the enum entry `symbol`, typed `ty`: the entry's own enum class.
    pub(super) fn enum_entry(&mut self, symbol: &KlibIrSymbol, ty: Ty) -> Lowered<ExprId> {
        if symbol.kind != KlibIrSymbolKind::EnumEntry {
            return Err(mismatch(
                "an enum entry read names a symbol that is not an enum entry",
            ));
        }
        let KlibIrSignature::Public(signature) = &symbol.signature else {
            return unsupported("a file-private enum entry");
        };
        if signature.member_id().is_some() {
            return Err(mismatch("an enum entry carries a member id"));
        }
        let (classifier, name) = self.linker.enum_entry(symbol)?;
        if name.is_empty() {
            return Err(mismatch("an enum entry has no name"));
        }
        let enum_type =
            crate::types::builtin_semantic(classifier).unwrap_or(Ty::obj_name(classifier));
        if ty != enum_type {
            return Err(mismatch("an enum entry is typed apart from its enum"));
        }
        let value = self.ir.add_expr(IrExpr::EnumEntry {
            classifier,
            name: name.into(),
        });
        self.ir.set_logical_type(value, enum_type);
        Ok(value)
    }
}
