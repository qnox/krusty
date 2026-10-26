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
        let mut roots = Vec::new();
        let mut functions = HashSet::default();
        functions.insert(function.base.symbol.clone());
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut roots);
        }
        self.emitted_calls_from(roots, closure, functions)
    }

    pub(crate) fn statement_expression_roots(
        &self,
        statements: &[KlibIrStatement],
    ) -> Vec<KlibIrExprId> {
        let mut roots = Vec::new();
        push_statement_expressions(self, statements, &mut roots);
        roots
    }

    /// Class initialization owns these roots; it uses the same reached-closure policy as bodies.
    pub(crate) fn emitted_callable_symbols_from(
        &self,
        roots: Vec<KlibIrExprId>,
        closure: fn(&KlibIrArena, &KlibIrExpr) -> Option<KlibIrFunctionId>,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_calls_from(roots, closure, HashSet::default())
    }

    fn emitted_calls_from(
        &self,
        mut pending: Vec<KlibIrExprId>,
        closure: fn(&KlibIrArena, &KlibIrExpr) -> Option<KlibIrFunctionId>,
        mut functions: HashSet<KlibIrSymbol>,
    ) -> Vec<&KlibIrSymbol> {
        let mut expressions = HashSet::default();
        let mut calls = self
            .callable_symbols_from(pending.clone())
            .collect::<Vec<_>>();
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
