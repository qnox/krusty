//! Normal-completion effects for compact signature extraction.
//!
//! This walker is deliberately narrower than the AST's structural child walker. A fact may escape
//! a statement only when every normal path through that statement evaluated the expression that
//! established it. Branch bodies, short-circuit right operands, safe-call arguments, lambda bodies,
//! and loop bodies therefore do not contribute facts to the following statement.

use crate::ast::{BinOp, Expr, ExprId, File, Stmt, StmtId, TemplatePart};

pub(super) fn not_null_assertions_after(file: &File, statement: StmtId) -> Vec<&str> {
    let mut names = Vec::new();
    match file.stmt(statement) {
        Stmt::Local { init, .. }
        | Stmt::LocalDelegate { delegate: init, .. }
        | Stmt::Destructure { init, .. }
        | Stmt::Assign { value: init, .. }
        | Stmt::Expr(init) => collect_expression(file, *init, &mut names),
        Stmt::AssignMember {
            receiver,
            value,
            safe,
            ..
        } => {
            collect_expression(file, *receiver, &mut names);
            if !safe {
                collect_expression(file, *value, &mut names);
            }
        }
        Stmt::AssignIndex {
            array,
            indices,
            value,
        } => {
            collect_expression(file, *array, &mut names);
            for index in indices {
                collect_expression(file, *index, &mut names);
            }
            collect_expression(file, *value, &mut names);
        }
        Stmt::While { cond, .. } => collect_expression(file, *cond, &mut names),
        Stmt::For { range, .. } => {
            collect_expression(file, range.start, &mut names);
            collect_expression(file, range.end, &mut names);
        }
        Stmt::ForEach { iterable, .. } => collect_expression(file, *iterable, &mut names),
        Stmt::CompoundAssign { target, value, .. } => {
            collect_expression(file, *target, &mut names);
            collect_expression(file, *value, &mut names);
        }
        // These statements either do not continue normally or contain bodies/conditions whose
        // evaluation is not guaranteed on every path that reaches the following statement.
        Stmt::LocalLateinit { .. }
        | Stmt::IncDec { .. }
        | Stmt::Return(..)
        | Stmt::Break(..)
        | Stmt::Continue(..)
        | Stmt::DoWhile { .. }
        | Stmt::LocalFun(_)
        | Stmt::LocalClass(_)
        | Stmt::LocalTypeAlias(_) => {}
    }
    names
}

fn collect_expression<'a>(file: &'a File, expression: ExprId, names: &mut Vec<&'a str>) {
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
        | Expr::Break { .. }
        | Expr::Continue { .. }
        | Expr::Lambda { .. }
        | Expr::Try { .. }
        | Expr::Block { .. } => {}
        Expr::AnnotationArrayLiteral(elements) => {
            for element in elements {
                collect_expression(file, *element, names);
            }
        }
        Expr::NotNull { operand } => {
            collect_expression(file, *operand, names);
            if let Expr::Name(name) = file.expr(*operand) {
                names.push(name);
            }
        }
        // Only the left side is unconditional: the right side runs only when the left is null.
        Expr::Elvis { lhs, .. } => collect_expression(file, *lhs, names),
        Expr::Template(parts) => {
            for part in parts {
                if let TemplatePart::Expr(expression) = part {
                    collect_expression(file, *expression, names);
                }
            }
        }
        // A null receiver skips both member dispatch and argument evaluation.
        Expr::SafeCall { receiver, .. } => collect_expression(file, *receiver, names),
        // No following statement is reachable when these expressions transfer control.
        Expr::Throw { .. } | Expr::Return { .. } => {}
        Expr::Is { operand, .. }
        | Expr::As { operand, .. }
        | Expr::IncDec {
            target: operand, ..
        }
        | Expr::Unary { operand, .. }
        | Expr::Member {
            receiver: operand, ..
        } => collect_expression(file, *operand, names),
        Expr::InRange {
            value, start, end, ..
        } => {
            collect_expression(file, *value, names);
            collect_expression(file, *start, names);
            collect_expression(file, *end, names);
        }
        Expr::RangeTo { lo, hi, .. } => {
            collect_expression(file, *lo, names);
            collect_expression(file, *hi, names);
        }
        Expr::Binary { op, lhs, rhs, .. } => {
            collect_expression(file, *lhs, names);
            if !matches!(op, BinOp::And | BinOp::Or) {
                collect_expression(file, *rhs, names);
            }
        }
        Expr::ExtensionAccess { receiver, callable } => {
            collect_expression(file, *receiver, names);
            collect_expression(file, *callable, names);
        }
        Expr::Index { array, indices } => {
            collect_expression(file, *array, names);
            for index in indices {
                collect_expression(file, *index, names);
            }
        }
        Expr::Call { callee, args } => {
            collect_expression(file, *callee, names);
            for argument in args {
                collect_expression(file, *argument, names);
            }
        }
        // The condition is the only expression common to both normal paths.
        Expr::If { cond, .. } => collect_expression(file, *cond, names),
        // A callable reference evaluates its bound receiver when it is constructed.
        Expr::CallableRef { receiver, .. } => {
            if let Some(receiver) = receiver {
                collect_expression(file, *receiver, names);
            }
        }
        // A subject is evaluated before choosing an arm; arm conditions and bodies are conditional.
        Expr::When { subject, .. } => {
            if let Some(subject) = subject {
                collect_expression(file, *subject, names);
            }
        }
    }
}
