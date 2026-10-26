//! Callable identities in selected bodies and their reached closure implementations.

use super::*;
use crate::fx_hash::HashSet;

/// The forms common lowering realizes differently from the calls they serialize: the closures
/// whose implementations it lowers with the body, and the calls it realizes as a common-IR
/// operation instead of an edge to the declaration they name. The policy is common lowering's;
/// the arena only walks.
#[derive(Clone, Copy)]
pub(crate) struct EmittedCallForms {
    pub(crate) closure: fn(&KlibIrArena, &KlibIrExpr) -> Option<KlibIrFunctionId>,
    pub(crate) folded_call: fn(&KlibIrArena, &KlibIrExpr) -> bool,
}

impl KlibIrArena {
    /// Closure selection belongs to common lowering; this arena supplies only the structural walk.
    /// Defaults and unreferenced local declarations are never roots. The direct-only inventories
    /// remain separate, particularly the one that authenticates intrinsic support.
    pub(crate) fn emitted_callable_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        let mut roots = Vec::new();
        let mut functions = HashSet::default();
        functions.insert(function.base.symbol.clone());
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut roots);
        }
        self.emitted_calls_from(roots, forms, functions)
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
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_calls_from(roots, forms, HashSet::default())
    }

    fn emitted_calls_from(
        &self,
        mut pending: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
        mut functions: HashSet<KlibIrSymbol>,
    ) -> Vec<&KlibIrSymbol> {
        let mut expressions = HashSet::default();
        let mut calls = self.unfolded_call_symbols(pending.clone(), forms.folded_call);
        while let Some(id) = pending.pop() {
            if !expressions.insert(id) {
                continue;
            }
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            if let Some(nested) = (forms.closure)(self, expression) {
                let nested = self.function(nested);
                if functions.insert(nested.base.symbol.clone()) {
                    if let Some(KlibIrBody::Block(statements)) = &nested.body {
                        let mut roots = Vec::new();
                        push_statement_expressions(self, statements, &mut roots);
                        calls.extend(self.unfolded_call_symbols(roots.clone(), forms.folded_call));
                        pending.extend(roots);
                    }
                }
            }
        }
        calls
    }

    /// The callable symbols reached from `pending`, except those of calls `folded` realizes.
    fn unfolded_call_symbols(
        &self,
        mut pending: Vec<KlibIrExprId>,
        folded: fn(&KlibIrArena, &KlibIrExpr) -> bool,
    ) -> Vec<&KlibIrSymbol> {
        let mut calls = Vec::new();
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            match &expression.kind {
                KlibIrExprKind::Call { access, .. } if !folded(self, expression) => {
                    calls.push(&access.symbol)
                }
                KlibIrExprKind::ConstructorCall { access, .. }
                | KlibIrExprKind::DelegatingConstructorCall(access)
                | KlibIrExprKind::EnumConstructorCall(access) => calls.push(&access.symbol),
                _ => {}
            }
        }
        calls
    }
}
