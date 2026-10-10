//! Decoded KLIB IR arenas built by hand, for the tests of the code that consumes them.

use super::tree::{
    KlibIrArena, KlibIrExpr, KlibIrExprId, KlibIrExprKind, KlibIrFunction, KlibIrFunctionId,
    KlibIrType, KlibIrTypeId,
};

fn next_id(length: usize) -> u32 {
    u32::try_from(length).expect("a fixture arena fits u32")
}

impl KlibIrArena {
    pub(crate) fn push_type(&mut self, ty: KlibIrType) -> KlibIrTypeId {
        self.types.push(ty);
        KlibIrTypeId(next_id(self.types.len() - 1))
    }

    pub(crate) fn push_expr(
        &mut self,
        ty: Option<KlibIrTypeId>,
        kind: KlibIrExprKind,
    ) -> KlibIrExprId {
        self.exprs.push(KlibIrExpr { ty, kind });
        KlibIrExprId(next_id(self.exprs.len() - 1))
    }

    pub(crate) fn push_function(&mut self, function: KlibIrFunction) -> KlibIrFunctionId {
        self.functions.push(function);
        KlibIrFunctionId(next_id(self.functions.len() - 1))
    }
}
