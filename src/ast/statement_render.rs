//! Statement rendering for the debug tree.
//!
//! Safe-index stores render with the rest of the statement forms. Expression rendering stays with
//! the arena module and calls back here.

use super::*;

impl File {
    pub(super) fn write_stmt(&self, id: StmtId, out: &mut String) {
        match self.stmt(id) {
            Stmt::Local {
                is_var, name, init, ..
            } => {
                out.push_str(&format!("({} {name} ", if *is_var { "var" } else { "val" }));
                self.write_expr(*init, out);
                out.push(')');
            }
            Stmt::LocalLateinit { name, .. } => {
                out.push_str(&format!("(lateinit var {name})"));
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                delegate,
                ..
            } => {
                out.push_str(&format!(
                    "({} {name} by ",
                    if *is_var { "var" } else { "val" }
                ));
                self.write_expr(*delegate, out);
                out.push(')');
            }
            Stmt::Destructure { entries, init } => {
                let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
                out.push_str(&format!("(destructure ({}) ", names.join(" ")));
                self.write_expr(*init, out);
                out.push(')');
            }
            Stmt::Assign { name, value } => {
                out.push_str(&format!("(set {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::IncDec { name, dec, .. } => {
                out.push_str(&format!("({} {name})", if *dec { "dec" } else { "inc" }));
            }
            Stmt::AssignMember {
                receiver,
                name,
                value,
                ..
            } => {
                out.push_str("(set-member ");
                self.write_expr(*receiver, out);
                out.push_str(&format!(" {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => {
                out.push_str(if indices.len() == 1 {
                    "(set-index "
                } else {
                    "(set-index-multi "
                });
                self.write_expr(*array, out);
                for &i in indices {
                    out.push(' ');
                    self.write_expr(i, out);
                }
                out.push(' ');
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::AssignSafeIndex {
                receiver,
                access,
                indices,
                value,
                ..
            } => {
                let receiver = *receiver;
                let access = *access;
                let indices = indices.clone();
                let value = *value;
                out.push_str("(set-safe-index ");
                self.write_expr(receiver, out);
                self.write_member_name(access, out);
                for index in indices {
                    out.push(' ');
                    self.write_expr(index, out);
                }
                out.push(' ');
                self.write_expr(value, out);
                out.push(')');
            }
            Stmt::Break(l) => out.push_str(&format!(
                "(break{})",
                l.as_ref().map(|s| format!("@{s}")).unwrap_or_default()
            )),
            Stmt::Continue(l) => out.push_str(&format!(
                "(continue{})",
                l.as_ref().map(|s| format!("@{s}")).unwrap_or_default()
            )),
            Stmt::Return(e, label) => {
                out.push_str("(return");
                if let Some(l) = label {
                    out.push_str(&format!("@{l}"));
                }
                if let Some(e) = e {
                    out.push(' ');
                    self.write_expr(*e, out);
                }
                out.push(')');
            }
            Stmt::While { cond, body, .. } => {
                out.push_str("(while ");
                self.write_expr(*cond, out);
                out.push(' ');
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::DoWhile { body, cond, .. } => {
                out.push_str("(do ");
                self.write_expr(*body, out);
                out.push_str(" while ");
                self.write_expr(*cond, out);
                out.push(')');
            }
            Stmt::For {
                name, range, body, ..
            } => {
                let op = match range.kind {
                    crate::ast::RangeKind::Through => "..",
                    crate::ast::RangeKind::OpenEnd => "..<",
                    crate::ast::RangeKind::Until => "until",
                    crate::ast::RangeKind::DownTo => "downTo",
                };
                out.push_str(&format!("(for {name} ("));
                self.write_expr(range.start, out);
                out.push_str(&format!(" {op} "));
                self.write_expr(range.end, out);
                out.push_str(") ");
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                out.push_str(&format!("(for-each {name} "));
                self.write_expr(*iterable, out);
                out.push(' ');
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::Expr(e) => self.write_expr(*e, out),
            Stmt::LocalFun(f) => {
                out.push_str(&format!("(local-fun {})", f.name));
            }
            Stmt::LocalClass(c) => {
                out.push_str(&format!("(local-class {})", c.name));
            }
            Stmt::LocalTypeAlias(alias) => {
                out.push_str(&format!(
                    "(local-typealias {} {})",
                    alias.name, alias.target.name
                ));
            }
            Stmt::CompoundAssign {
                target, value, op, ..
            } => {
                out.push_str(&format!("(compound-{} ", binop(*op)));
                self.write_expr(*target, out);
                out.push(' ');
                self.write_expr(*value, out);
                out.push(')');
            }
        }
    }
}
