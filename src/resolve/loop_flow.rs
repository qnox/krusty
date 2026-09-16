//! Loop back-edge invalidation and the nested-write scan it shares with capture analysis.

use std::cell::RefCell;
use std::collections::HashSet;

use crate::ast::{Expr, ExprId, File, Stmt, StmtId};

use super::{local_class_capture_expressions, Checker, CheckerScope};

/// Collect every name reassigned (`=`, compound assignment, or inc/dec) anywhere in an expression,
/// including nested lambdas, local functions, and parser-hoisted local-class member bodies.
pub(super) fn collect_all_reassigned(file: &File, expression: ExprId, out: &mut HashSet<String>) {
    let cell = RefCell::new(std::mem::take(out));
    fn visit_expression(file: &File, expression: ExprId, cell: &RefCell<HashSet<String>>) {
        if let Expr::IncDec { target, .. } = file.expr(expression) {
            if let Expr::Name(name) = file.expr(*target) {
                cell.borrow_mut().insert(name.clone());
            }
        }
        file.any_child_expr(
            expression,
            &mut |child| {
                visit_expression(file, child, cell);
                false
            },
            &mut |statement| {
                visit_statement(file, statement, cell);
                false
            },
        );
    }
    fn visit_statement(file: &File, statement: StmtId, cell: &RefCell<HashSet<String>>) {
        if let Stmt::Assign { name, .. } | Stmt::IncDec { name, .. } = file.stmt(statement) {
            cell.borrow_mut().insert(name.clone());
        }
        if let Stmt::LocalClass(class) = file.stmt(statement) {
            for expression in local_class_capture_expressions(class) {
                visit_expression(file, expression, cell);
            }
        }
        file.any_child_stmt(statement, &mut |child| {
            visit_expression(file, child, cell);
            false
        });
    }
    visit_expression(file, expression, &cell);
    *out = cell.into_inner();
}

impl Checker<'_> {
    /// Drop flow proofs invalidated by a loop's back-edge writes before checking its condition.
    ///
    /// The enclosing scope must lose those proofs so the condition, body, and continuation all see
    /// the back edge. A proof established by the condition is applied afterwards and therefore
    /// remains valid on each body entry.
    pub(super) fn clear_narrowings_a_loop_invalidates(
        &mut self,
        scope: &CheckerScope<'_>,
        parts: &[ExprId],
    ) {
        let mut written = HashSet::new();
        for &part in parts {
            collect_all_reassigned(self.file, part, &mut written);
        }
        for name in &written {
            self.set_local_narrow(scope, name, None);
        }
    }
}
