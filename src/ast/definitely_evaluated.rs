//! Expressions evaluated on every normally completing path.
//!
//! This is deliberately narrower than the AST's structural child walker. A phase may publish a
//! fact only when every normal path through the containing expression or statement evaluated the
//! node that established it. Branch bodies, short-circuit right operands, safe-call selectors,
//! lambda bodies, and loop bodies therefore do not contribute facts to the continuation.

use super::{BinOp, Expr, ExprId, File, Stmt, StmtId, TemplatePart};

/// Visit expressions in evaluation order when every normally completing path through `statement`
/// evaluated them. The callback attaches phase-owned facts; this AST boundary owns only traversal.
pub(crate) fn for_each_in_statement(
    file: &File,
    statement: StmtId,
    visit: &mut impl FnMut(ExprId),
) {
    let mut evaluated = Vec::new();
    if collect_statement(file, statement, &mut evaluated) {
        evaluated.into_iter().for_each(visit);
    }
}

fn collect_statement(file: &File, statement: StmtId, evaluated: &mut Vec<ExprId>) -> bool {
    match file.stmt(statement) {
        Stmt::Local { init, .. }
        | Stmt::LocalDelegate { delegate: init, .. }
        | Stmt::Destructure { init, .. }
        | Stmt::Assign { value: init, .. }
        | Stmt::Expr(init) => collect_expression(file, *init, evaluated),
        Stmt::AssignMember {
            receiver,
            value,
            safe,
            ..
        } => {
            collect_expression(file, *receiver, evaluated)
                && (*safe || collect_expression(file, *value, evaluated))
        }
        Stmt::AssignIndex {
            array,
            indices,
            value,
        } => {
            collect_expression(file, *array, evaluated)
                && indices
                    .iter()
                    .all(|index| collect_expression(file, *index, evaluated))
                && collect_expression(file, *value, evaluated)
        }
        // The index and the assigned value run only when the receiver is non-null.
        Stmt::AssignSafeIndex { receiver, .. } => collect_expression(file, *receiver, evaluated),
        Stmt::While { cond, .. } => collect_expression(file, *cond, evaluated),
        Stmt::For { range, .. } => {
            collect_expression(file, range.start, evaluated)
                && collect_expression(file, range.end, evaluated)
        }
        Stmt::ForEach { iterable, .. } => collect_expression(file, *iterable, evaluated),
        Stmt::CompoundAssign { target, value, .. } => {
            collect_expression(file, *target, evaluated)
                && collect_expression(file, *value, evaluated)
        }
        // These statements either do not continue normally or contain bodies/conditions whose
        // evaluation is not guaranteed on every path that reaches the following statement.
        Stmt::Return(..) | Stmt::Break(..) | Stmt::Continue(..) => false,
        Stmt::LocalLateinit { .. }
        | Stmt::IncDec { .. }
        | Stmt::DoWhile { .. }
        | Stmt::LocalFun(_)
        | Stmt::LocalClass(_)
        | Stmt::LocalTypeAlias(_) => true,
    }
}

/// Visit expressions in evaluation order when every normally completing path through `expression`
/// evaluated them. The expression itself is visited after its guaranteed children.
pub(crate) fn for_each_in_expression(
    file: &File,
    expression: ExprId,
    visit: &mut impl FnMut(ExprId),
) {
    let mut evaluated = Vec::new();
    if collect_expression(file, expression, &mut evaluated) {
        evaluated.into_iter().for_each(visit);
    }
}

fn collect_expression(file: &File, expression: ExprId, evaluated: &mut Vec<ExprId>) -> bool {
    match file.expr(expression) {
        Expr::IntLit(_)
        | Expr::LongLit(_)
        | Expr::UIntLit(_)
        | Expr::ULongLit(_)
        | Expr::DoubleLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::CharLit(_)
        | Expr::NullLit
        | Expr::UnsupportedAnnotationArgument(_)
        | Expr::Name(_)
        | Expr::Lambda { .. }
        | Expr::Try { .. }
        | Expr::Block { .. } => {}
        Expr::Break { .. } | Expr::Continue { .. } | Expr::Throw { .. } | Expr::Return { .. } => {
            return false;
        }
        Expr::AnnotationArrayLiteral(elements) => {
            if !elements
                .iter()
                .all(|element| collect_expression(file, *element, evaluated))
            {
                return false;
            }
        }
        Expr::NotNull { operand } => {
            if !collect_expression(file, *operand, evaluated) {
                return false;
            }
        }
        // Only the left side is unconditional: the right side runs only when the left is null.
        Expr::Elvis { lhs, .. } => {
            if !collect_expression(file, *lhs, evaluated) {
                return false;
            }
        }
        Expr::Template(parts) => {
            for part in parts {
                if let TemplatePart::Expr(part) = part {
                    if !collect_expression(file, *part, evaluated) {
                        return false;
                    }
                }
            }
        }
        // A null receiver skips both member dispatch and argument evaluation.
        Expr::SafeCall { receiver, .. }
        | Expr::SafeIndex { receiver, .. }
        | Expr::SafeIndexIncDec { receiver, .. } => {
            if !collect_expression(file, *receiver, evaluated) {
                return false;
            }
        }
        Expr::Is { operand, .. }
        | Expr::As { operand, .. }
        | Expr::IncDec {
            target: operand, ..
        }
        | Expr::Unary { operand, .. }
        | Expr::Member {
            receiver: operand, ..
        } => {
            if !collect_expression(file, *operand, evaluated) {
                return false;
            }
        }
        Expr::InRange {
            value, start, end, ..
        } => {
            if !collect_expression(file, *value, evaluated)
                || !collect_expression(file, *start, evaluated)
                || !collect_expression(file, *end, evaluated)
            {
                return false;
            }
        }
        Expr::RangeTo { lo, hi, .. } => {
            if !collect_expression(file, *lo, evaluated)
                || !collect_expression(file, *hi, evaluated)
            {
                return false;
            }
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            if !collect_expression(file, *lhs, evaluated)
                || (!matches!(op, BinOp::And | BinOp::Or)
                    && !collect_expression(file, *rhs, evaluated))
            {
                return false;
            }
        }
        Expr::ExtensionAccess { receiver, callable } => {
            if !collect_expression(file, *receiver, evaluated)
                || !collect_expression(file, *callable, evaluated)
            {
                return false;
            }
        }
        Expr::Index { array, indices } => {
            if !collect_expression(file, *array, evaluated)
                || !indices
                    .iter()
                    .all(|index| collect_expression(file, *index, evaluated))
            {
                return false;
            }
        }
        Expr::Call { callee, args } => {
            if !collect_expression(file, *callee, evaluated)
                || !args
                    .iter()
                    .all(|argument| collect_expression(file, *argument, evaluated))
            {
                return false;
            }
        }
        // The condition is the only expression common to both normal paths.
        Expr::If { cond, .. } => {
            if !collect_expression(file, *cond, evaluated) {
                return false;
            }
        }
        // A callable reference evaluates its bound receiver when it is constructed.
        Expr::CallableRef { receiver, .. } => {
            if let Some(receiver) = receiver {
                if !collect_expression(file, *receiver, evaluated) {
                    return false;
                }
            }
        }
        // A subject is evaluated before choosing an arm; arm conditions and bodies are conditional.
        Expr::When { subject, .. } => {
            if let Some(subject) = subject {
                if !collect_expression(file, *subject, evaluated) {
                    return false;
                }
            }
        }
    }
    evaluated.push(expression);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Span;

    fn add(file: &mut File, expression: Expr) -> ExprId {
        file.add_expr(expression, Span::new(0, 0))
    }

    fn visited(file: &File, root: ExprId) -> Vec<ExprId> {
        let mut visited = Vec::new();
        for_each_in_expression(file, root, &mut |expression| visited.push(expression));
        visited
    }

    #[test]
    fn nested_normal_completion_forms_visit_their_unconditional_operands() {
        let mut file = File::default();
        let value = add(&mut file, Expr::Name("value".into()));
        let asserted = add(&mut file, Expr::NotNull { operand: value });
        let callable = add(&mut file, Expr::Name("callable".into()));

        let template = add(
            &mut file,
            Expr::Template(vec![TemplatePart::Expr(asserted)]),
        );
        assert_eq!(visited(&file, template), vec![value, asserted, template]);

        let extension = add(
            &mut file,
            Expr::ExtensionAccess {
                receiver: asserted,
                callable,
            },
        );
        assert_eq!(
            visited(&file, extension),
            vec![value, asserted, callable, extension]
        );

        let reference = add(
            &mut file,
            Expr::CallableRef {
                receiver: Some(asserted),
                name: "read".into(),
            },
        );
        assert_eq!(visited(&file, reference), vec![value, asserted, reference]);

        let when = add(
            &mut file,
            Expr::When {
                subject: Some(asserted),
                arms: Vec::new(),
            },
        );
        assert_eq!(visited(&file, when), vec![value, asserted, when]);

        let incdec = add(
            &mut file,
            Expr::IncDec {
                target: asserted,
                dec: false,
                prefix: false,
            },
        );
        assert_eq!(visited(&file, incdec), vec![value, asserted, incdec]);
    }

    #[test]
    fn short_circuit_and_non_completion_do_not_publish_conditional_facts() {
        let mut file = File::default();
        let lhs = add(&mut file, Expr::BoolLit(false));
        let value = add(&mut file, Expr::Name("value".into()));
        let asserted = add(&mut file, Expr::NotNull { operand: value });
        let short_circuit = add(
            &mut file,
            Expr::Binary {
                op: BinOp::And,
                lhs,
                rhs: asserted,
                operator_span: Span::new(0, 0),
            },
        );
        assert_eq!(visited(&file, short_circuit), vec![lhs, short_circuit]);

        let thrown = add(&mut file, Expr::Throw { operand: value });
        let eager = add(
            &mut file,
            Expr::Binary {
                op: BinOp::Add,
                lhs: asserted,
                rhs: thrown,
                operator_span: Span::new(0, 0),
            },
        );
        assert!(visited(&file, eager).is_empty());
    }
}
