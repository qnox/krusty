//! Callable identities in selected bodies and their reached closure implementations.

use super::*;
use crate::fx_hash::HashSet;

impl KlibIrArena {
    /// Closure selection belongs to common lowering; this arena supplies only the structural walk.
    /// Defaults and unreferenced local declarations are never roots. The direct-only inventories
    /// remain separate, particularly the one that authenticates intrinsic support.
    pub(crate) fn emitted_callable_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        closure: fn(&KlibIrArena, &KlibIrExpr) -> Option<KlibIrFunctionId>,
    ) -> Vec<&'a KlibIrSymbol> {
        let mut pending = Vec::new();
        let mut functions = HashSet::default();
        let mut expressions = HashSet::default();
        let mut calls = self.callable_symbols_in_body(function).collect::<Vec<_>>();
        functions.insert(function.base.symbol.clone());
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut pending);
        }
        while let Some(id) = pending.pop() {
            if !expressions.insert(id) {
                continue;
            }
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            if let Some(nested) = closure(self, expression) {
                let nested = self.function(nested);
                if functions.insert(nested.base.symbol.clone()) {
                    calls.extend(self.callable_symbols_in_body(nested));
                    if let Some(KlibIrBody::Block(statements)) = &nested.body {
                        push_statement_expressions(self, statements, &mut pending);
                    }
                }
            }
        }
        calls
    }
}
