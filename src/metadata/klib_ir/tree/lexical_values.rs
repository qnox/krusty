//! Value references in the lexical executable graph, including nested closure bodies.

use super::*;
use crate::fx_hash::HashSet;

impl KlibIrArena {
    /// Value symbols mentioned by a body and its reachable nested closure declarations, in
    /// traversal order. These are references, not a capture decision: the consumer joins them
    /// against its enclosing declaration's value environment by exact serialized identity.
    pub(crate) fn lexical_value_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
    ) -> Vec<&'a KlibIrSymbol> {
        let mut pending = Vec::new();
        let mut functions = HashSet::default();
        let mut expressions = HashSet::default();
        let mut values = Vec::new();
        self.push_lexical_body(function, &mut functions, &mut pending);
        while let Some(id) = pending.pop() {
            if !expressions.insert(id) {
                continue;
            }
            let expression = self.expr(id);
            let start = pending.len();
            push_child_expressions(self, &expression.kind, &mut pending);
            pending[start..].reverse();
            match &expression.kind {
                KlibIrExprKind::GetValue { symbol, .. }
                | KlibIrExprKind::SetValue { symbol, .. } => values.push(symbol),
                KlibIrExprKind::FunctionExpression { function, .. }
                | KlibIrExprKind::RichFunctionReference {
                    invoke: function, ..
                } => {
                    self.push_lexical_body(self.function(*function), &mut functions, &mut pending);
                }
                _ => {}
            }
        }
        values
    }

    fn push_lexical_body(
        &self,
        function: &KlibIrFunction,
        functions: &mut HashSet<KlibIrSymbol>,
        pending: &mut Vec<KlibIrExprId>,
    ) {
        if !functions.insert(function.base.symbol.clone()) {
            return;
        }
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            let start = pending.len();
            push_statement_expressions(self, statements, pending);
            pending[start..].reverse();
        }
    }
}
