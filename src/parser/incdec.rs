//! Safe increment/decrement access expansion.
//!
//! A safe property update evaluates its receiver once and guards the complete read, operator call,
//! and write. Keeping this shape together prevents nullable operator selection and eager updates.

use super::*;

impl Parser<'_> {
    /// Build the store half shared by discarded-value and value-producing member/index inc/dec.
    /// Centralizing this match keeps the accepted lvalue families, source spans, and future property
    /// or index-store extensions identical across both syntactic contexts.
    pub(super) fn finish_incdec_access_assignment(
        &mut self,
        target: ExprId,
        value: ExprId,
        start: Span,
        target_span: Span,
    ) -> Option<StmtId> {
        let statement = match self.file.expr(target).clone() {
            Expr::Member { receiver, name } => Stmt::AssignMember {
                receiver,
                name,
                value,
                safe: false,
            },
            Expr::SafeCall {
                receiver,
                name,
                args: None,
            } => Stmt::AssignMember {
                receiver,
                name,
                value,
                safe: true,
            },
            Expr::Index { array, indices } => Stmt::AssignIndex {
                array,
                indices,
                value,
            },
            _ => return None,
        };
        Some(self.finish_assignment_stmt(statement, start, target_span))
    }

    pub(super) fn incdec_target(
        &mut self,
        e: ExprId,
        dec: bool,
        prefix: bool,
        op_span: Span,
        start: Span,
    ) -> StmtId {
        let target_span = self.assignment_target_span(e);
        match self.file.expr(e).clone() {
            Expr::Name(n) => self.parse_incdec(n, dec, prefix, start, target_span),
            _ => {
                self.diags.error(
                    op_span,
                    "krusty: '++'/'--' is only supported on a variable, property, or indexed access",
                );
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    pub(super) fn parse_incdec(
        &mut self,
        name: String,
        dec: bool,
        prefix: bool,
        start: Span,
        target: Span,
    ) -> StmtId {
        self.finish_assignment_stmt(Stmt::IncDec { name, dec, prefix }, start, target)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn safe_incdec_access_value_expr(
        &mut self,
        e: ExprId,
        target: ExprId,
        dec: bool,
        prefix: bool,
        start: Span,
        target_span: Span,
    ) -> Option<ExprId> {
        let Expr::SafeCall {
            receiver,
            name,
            args: None,
        } = self.file.expr(target).clone()
        else {
            return None;
        };
        const RECEIVER: &str = "$$incDecReceiver";
        let receiver_local = self.file.add_stmt(
            Stmt::Local {
                is_var: false,
                name: RECEIVER.to_string(),
                ty: None,
                init: receiver,
            },
            start,
        );
        let receiver_read = self
            .file
            .add_expr(Expr::Name(RECEIVER.to_string()), target_span);
        let member = self.file.add_expr(
            Expr::Member {
                receiver: receiver_read,
                name,
            },
            target_span,
        );
        let update = self.incdec_access_value_expr(e, member, dec, prefix, start);

        let condition_receiver = self
            .file
            .add_expr(Expr::Name(RECEIVER.to_string()), target_span);
        let null = self.file.add_expr(Expr::NullLit, target_span);
        let condition = self.file.add_expr(
            Expr::Binary {
                op: BinOp::Ne,
                lhs: condition_receiver,
                rhs: null,
                operator_span: target_span,
            },
            target_span,
        );
        let null_result = self.file.add_expr(Expr::NullLit, target_span);
        let guarded = self.file.add_expr(
            Expr::If {
                cond: condition,
                then_branch: update,
                else_branch: Some(null_result),
            },
            target_span,
        );
        Some(self.file.add_expr(
            Expr::Block {
                stmts: vec![receiver_local],
                trailing: Some(guarded),
            },
            Span::new(start.lo, self.file.expr_spans[e.0 as usize].hi),
        ))
    }

    /// Drop a postfix access increment's saved old value when it is used as a statement. A prefix
    /// re-read stays, as a statement: kotlinc calls the getter or `get` again even when the result
    /// is discarded. Safe updates have no operand record on their guarded outer block, so the
    /// conditional stays live.
    pub(super) fn discard_incdec_access_value(&mut self, expression: ExprId) -> bool {
        if !self.file.incdec_access_operands.contains_key(&expression) {
            return false;
        }
        let Expr::Block { stmts, trailing } = self.file.expr(expression).clone() else {
            return false;
        };
        let Some(trailing) = trailing else {
            return false;
        };
        let keeps_reread = !matches!(
            self.file.expr(trailing),
            Expr::Name(name) if name == "$$incDecOriginal"
        );
        let original = stmts.iter().position(|&statement| {
            matches!(
                self.file.stmt(statement),
                Stmt::Local { name, .. } if name == "$$incDecOriginal"
            )
        });
        let mut stmts = stmts;
        if let Some(original) = original {
            stmts.remove(original);
        }
        let reread = keeps_reread.then(|| {
            let span = self.file.expr_spans[trailing.0 as usize];
            self.file.add_stmt(Stmt::Expr(trailing), span)
        });
        let Expr::Block {
            stmts: slot,
            trailing: trail,
        } = &mut self.file.expr_arena[expression.0 as usize]
        else {
            return false;
        };
        *slot = stmts;
        if let Some(reread) = reread {
            slot.push(reread);
        }
        *trail = None;
        true
    }

    /// Expand a member/index increment into an `Expr::Block`, letting it retain its value in an
    /// initializer, argument, return, or larger expression. Receiver and index operands are saved
    /// left-to-right before the read, so calls and custom access paths are evaluated exactly once.
    /// A prefix result is a second read of that same access; a postfix result is the first read.
    pub(super) fn incdec_access_value_expr(
        &mut self,
        e: ExprId,
        target: ExprId,
        dec: bool,
        prefix: bool,
        start: Span,
    ) -> ExprId {
        let target_span = self.assignment_target_span(target);
        if let Some(guarded) =
            self.safe_incdec_access_value_expr(e, target, dec, prefix, start, target_span)
        {
            return guarded;
        }
        let Some((operands, target_read, target_write)) =
            self.share_incdec_access_target(target, target_span)
        else {
            self.diags.error(
                self.file.expr_spans[e.0 as usize],
                "krusty: '++'/'--' is only supported on a variable, property, or indexed access",
            );
            // Recover with the already-parsed target. Keeping the invalid `Expr::IncDec` reachable
            // would make statement parsing and resolution report the same error again.
            return target;
        };
        // Keep the working value in the parser's synthetic-name namespace. Even an escaped source
        // binding with the same spelling cannot interfere: this generated block contains only its
        // own local and generated reads, and repeated expansions have independent scopes.
        const VALUE: &str = "$$incDecValue";
        const ORIGINAL: &str = "$$incDecOriginal";
        let mut statements = Vec::new();
        let value_local = self.file.add_stmt(
            Stmt::Local {
                is_var: true,
                name: VALUE.to_string(),
                ty: None,
                init: target_read,
            },
            start,
        );
        statements.push(value_local);
        let original_read = if prefix {
            None
        } else {
            let original_init = self
                .file
                .add_expr(Expr::Name(VALUE.to_string()), target_span);
            statements.push(self.file.add_stmt(
                Stmt::Local {
                    is_var: false,
                    name: ORIGINAL.to_string(),
                    ty: None,
                    init: original_init,
                },
                start,
            ));
            Some(
                self.file
                    .add_expr(Expr::Name(ORIGINAL.to_string()), target_span),
            )
        };

        // Preserve the canonical inc/dec node on a lexical variable. The resolver therefore owns
        // operator-convention validation and exact overload selection instead of seeing an ordinary
        // `.inc()` call manufactured by the parser.
        statements.push(self.parse_incdec(VALUE.to_string(), dec, prefix, start, target_span));
        let assigned_value = self
            .file
            .add_expr(Expr::Name(VALUE.to_string()), target_span);
        let assignment = self
            .finish_incdec_access_assignment(target_write, assigned_value, start, target_span)
            .expect("cached member/index target has an assignment form");
        statements.push(assignment);
        let trailing = if prefix {
            self.share_incdec_access_target(target, target_span)
                .map(|(_, read, _)| read)
                .expect("the first share already accepted this access")
        } else {
            original_read.expect("postfix keeps the first read")
        };
        let block = self.file.add_expr(
            Expr::Block {
                stmts: statements,
                trailing: Some(trailing),
            },
            Span::new(start.lo, self.file.expr_spans[e.0 as usize].hi),
        );
        self.file.incdec_access_operands.insert(block, operands);
        block
    }

    /// Share the receiver/index expression IDs between the read and write halves of a value-producing
    /// access inc/dec. The block records those operands for lowering, where resolved value types can
    /// distinguish runtime expressions (spill once) from package/classifier/`super` qualifiers.
    fn share_incdec_access_target(
        &mut self,
        target: ExprId,
        span: Span,
    ) -> Option<(Vec<ExprId>, ExprId, ExprId)> {
        match self.file.expr(target).clone() {
            Expr::Member { receiver, name } => {
                let read = self.file.add_expr(
                    Expr::Member {
                        receiver,
                        name: name.clone(),
                    },
                    span,
                );
                let write = self.file.add_expr(Expr::Member { receiver, name }, span);
                Some((vec![receiver], read, write))
            }
            Expr::SafeCall {
                receiver,
                name,
                args: None,
            } => {
                let read = self.file.add_expr(
                    Expr::SafeCall {
                        receiver,
                        name: name.clone(),
                        args: None,
                    },
                    span,
                );
                let write = self.file.add_expr(
                    Expr::SafeCall {
                        receiver,
                        name,
                        args: None,
                    },
                    span,
                );
                Some((vec![receiver], read, write))
            }
            Expr::Index { array, indices } => {
                let mut operands = Vec::with_capacity(indices.len() + 1);
                operands.push(array);
                operands.extend(indices.iter().copied());
                let read = self.file.add_expr(
                    Expr::Index {
                        array,
                        indices: indices.clone(),
                    },
                    span,
                );
                let write = self.file.add_expr(Expr::Index { array, indices }, span);
                Some((operands, read, write))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn tree(source: &str) -> String {
        let mut diagnostics = DiagSink::new();
        let tokens = lex(source, &mut diagnostics);
        let file = parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "unexpected parse errors: {}",
            diagnostics.render("test", source)
        );
        file.debug_tree()
    }

    #[test]
    fn prefix_member_expansion_rereads_while_postfix_keeps_the_first_read() {
        assert_eq!(
            tree("fun f(probe: SyntheticProbe): Int = ++probe.value"),
            "(fun f (param probe SyntheticProbe) :Int (block (var $$incDecValue (. probe value)) (inc $$incDecValue) (set-member probe value $$incDecValue) =>(. probe value)))\n"
        );
        assert_eq!(
            tree("fun f(probe: SyntheticProbe): Int = probe.value++"),
            "(fun f (param probe SyntheticProbe) :Int (block (var $$incDecValue (. probe value)) (val $$incDecOriginal $$incDecValue) (inc $$incDecValue) (set-member probe value $$incDecValue) =>$$incDecOriginal))\n"
        );
    }
}
