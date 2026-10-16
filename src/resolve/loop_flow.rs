//! Loop back-edge invalidation and the nested-write scan it shares with capture analysis.

use crate::fx_hash::{HashMap, HashSet};
use std::cell::RefCell;

use crate::ast::{Expr, ExprId, File, Stmt, StmtId};

use super::scope::{Ns, PathRoot};
use super::{local_class_capture_expressions, Checker, CheckerScope};

/// Resolve a loop's writes to the exact bindings visible at its entry. Definite replacements are
/// separate from mutable compound assignments: the latter stay provisional until ordinary
/// operator selection decides whether they are in-place `opAssign` calls. The syntax scan runs
/// before body checking, so body declarations are removed at their lexical declaration point.
fn collect_loop_entry_writes(
    file: &File,
    parts: LoopParts<'_>,
    entry: &HashMap<String, LoopEntryBinding>,
) -> LoopEntryWrites {
    let written = RefCell::new(HashSet::default());
    let compound = RefCell::new(HashMap::default());

    fn visit_block(
        file: &File,
        statements: &[StmtId],
        trailing: Option<ExprId>,
        entry: &HashMap<String, LoopEntryBinding>,
        written: &RefCell<HashSet<u32>>,
        compound: &RefCell<HashMap<u32, usize>>,
    ) -> HashMap<String, LoopEntryBinding> {
        let mut visible = entry.clone();
        for &statement in statements {
            visit_statement(file, statement, &mut visible, written, compound);
        }
        if let Some(trailing) = trailing {
            visit_expression(file, trailing, &visible, written, compound);
        }
        visible
    }

    fn visit_expression(
        file: &File,
        expression: ExprId,
        entry: &HashMap<String, LoopEntryBinding>,
        written: &RefCell<HashSet<u32>>,
        compound: &RefCell<HashMap<u32, usize>>,
    ) {
        match file.expr(expression) {
            Expr::Block { stmts, trailing } => {
                visit_block(file, stmts, *trailing, entry, written, compound);
            }
            Expr::Lambda { params, body } => {
                let mut visible = entry.clone();
                if params.is_empty() {
                    if !file.lambda_explicit_arrows.contains(&expression.0) {
                        visible.remove("it");
                    }
                } else {
                    for parameter in params {
                        visible.remove(parameter);
                    }
                }
                visit_expression(file, *body, &visible, written, compound);
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                visit_expression(file, *body, entry, written, compound);
                for catch in catches {
                    let mut visible = entry.clone();
                    visible.remove(&catch.name);
                    visit_expression(file, catch.body, &visible, written, compound);
                }
                if let Some(finally) = finally {
                    visit_expression(file, *finally, entry, written, compound);
                }
            }
            Expr::IncDec { target, .. } => {
                if let Expr::Name(name) = file.expr(*target) {
                    if let Some(binding) = entry.get(name) {
                        written.borrow_mut().insert(binding.flow_identity);
                    }
                }
                visit_expression(file, *target, entry, written, compound);
            }
            _ => {
                file.any_child_expr(
                    expression,
                    &mut |child| {
                        visit_expression(file, child, entry, written, compound);
                        false
                    },
                    &mut |statement| {
                        let mut visible = entry.clone();
                        visit_statement(file, statement, &mut visible, written, compound);
                        false
                    },
                );
            }
        }
    }

    fn visit_statement(
        file: &File,
        statement: StmtId,
        visible: &mut HashMap<String, LoopEntryBinding>,
        written: &RefCell<HashSet<u32>>,
        compound: &RefCell<HashMap<u32, usize>>,
    ) {
        match file.stmt(statement) {
            Stmt::Assign { name, value } => {
                let is_compound = !file.value_operator_spans.contains_key(&value.0)
                    && matches!(
                        file.expr(*value),
                        Expr::Binary { lhs, .. }
                            if matches!(file.expr(*lhs), Expr::Name(lhs) if lhs == name)
                    );
                if let Some(binding) = visible.get(name) {
                    if is_compound && binding.reassignable {
                        *compound
                            .borrow_mut()
                            .entry(binding.flow_identity)
                            .or_default() += 1;
                    } else if !is_compound {
                        written.borrow_mut().insert(binding.flow_identity);
                    }
                }
                visit_expression(file, *value, visible, written, compound);
            }
            Stmt::IncDec { name, .. } => {
                if let Some(binding) = visible.get(name) {
                    written.borrow_mut().insert(binding.flow_identity);
                }
            }
            Stmt::Local { name, init, .. } => {
                visit_expression(file, *init, visible, written, compound);
                visible.remove(name);
            }
            Stmt::LocalLateinit { name, .. } => {
                visible.remove(name);
            }
            Stmt::LocalDelegate { name, delegate, .. } => {
                visit_expression(file, *delegate, visible, written, compound);
                visible.remove(name);
            }
            Stmt::Destructure { entries, init } => {
                visit_expression(file, *init, visible, written, compound);
                for entry in entries.iter().filter(|entry| !entry.ignored) {
                    visible.remove(&entry.name);
                }
            }
            Stmt::For {
                name, range, body, ..
            } => {
                visit_expression(file, range.start, visible, written, compound);
                visit_expression(file, range.end, visible, written, compound);
                let mut body_visible = visible.clone();
                body_visible.remove(name);
                visit_expression(file, *body, &body_visible, written, compound);
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                visit_expression(file, *iterable, visible, written, compound);
                let mut body_visible = visible.clone();
                body_visible.remove(name);
                visit_expression(file, *body, &body_visible, written, compound);
            }
            Stmt::DoWhile { body, cond, .. } => {
                if let Expr::Block { stmts, trailing } = file.expr(*body) {
                    let condition_visible =
                        visit_block(file, stmts, *trailing, visible, written, compound);
                    visit_expression(file, *cond, &condition_visible, written, compound);
                } else {
                    visit_expression(file, *body, visible, written, compound);
                    visit_expression(file, *cond, visible, written, compound);
                }
            }
            Stmt::LocalFun(function) => {
                let mut function_visible = visible.clone();
                for parameter in &function.params {
                    if let Some(default) = parameter.default {
                        visit_expression(file, default, &function_visible, written, compound);
                    }
                    function_visible.remove(&parameter.name);
                }
                if let crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body) =
                    &function.body
                {
                    visit_expression(file, *body, &function_visible, written, compound);
                }
            }
            Stmt::LocalClass(class) => {
                let mut member_visible = visible.clone();
                for property in class.props.iter().filter(|property| property.is_property) {
                    member_visible.remove(&property.name);
                }
                for property in &class.body_props {
                    member_visible.remove(&property.name);
                }
                for parameter in &class.context_params {
                    member_visible.remove(&parameter.name);
                }

                let mut scoped_bodies = HashSet::default();
                for method in &class.methods {
                    let mut method_visible = member_visible.clone();
                    for parameter in &method.params {
                        if let Some(default) = parameter.default {
                            scoped_bodies.insert(default);
                            visit_expression(file, default, &method_visible, written, compound);
                        }
                        method_visible.remove(&parameter.name);
                    }
                    if let crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body) =
                        &method.body
                    {
                        scoped_bodies.insert(*body);
                        visit_expression(file, *body, &method_visible, written, compound);
                    }
                }
                for property in &class.body_props {
                    if let Some(
                        crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body),
                    ) = property.getter.as_ref()
                    {
                        scoped_bodies.insert(*body);
                        visit_expression(file, *body, &member_visible, written, compound);
                    }
                    if let Some(setter) = property.setter.as_ref() {
                        if let Some(
                            crate::ast::FunBody::Expr(body) | crate::ast::FunBody::Block(body),
                        ) = setter.body.as_ref()
                        {
                            scoped_bodies.insert(*body);
                            let mut setter_visible = member_visible.clone();
                            setter_visible.remove(setter.param.as_deref().unwrap_or("value"));
                            visit_expression(file, *body, &setter_visible, written, compound);
                        }
                    }
                }

                // A constructor parameter enters scope only after its own default is checked.
                // Preserve an outer same-spelled value for that default, then hide it from every
                // later default. The completed constructor environment below still hides every
                // parameter from base arguments and instance initialization.
                let mut primary_default_visible = visible.clone();
                for parameter in &class.context_params {
                    primary_default_visible.remove(&parameter.name);
                }
                for parameter in &class.props {
                    if let Some(default) = parameter.default {
                        scoped_bodies.insert(default);
                        visit_expression(
                            file,
                            default,
                            &primary_default_visible,
                            written,
                            compound,
                        );
                    }
                    primary_default_visible.remove(&parameter.name);
                }

                for constructor in &class.secondary_ctors {
                    let mut constructor_visible = member_visible.clone();
                    for parameter in &constructor.params {
                        if let Some(default) = parameter.default {
                            scoped_bodies.insert(default);
                            visit_expression(
                                file,
                                default,
                                &constructor_visible,
                                written,
                                compound,
                            );
                        }
                        constructor_visible.remove(&parameter.name);
                    }
                    if let Some(body) = constructor.body {
                        scoped_bodies.insert(body);
                        visit_expression(file, body, &constructor_visible, written, compound);
                    }
                    let delegation_arguments = match &constructor.delegation {
                        crate::ast::CtorDelegation::None => &[][..],
                        crate::ast::CtorDelegation::This(call)
                        | crate::ast::CtorDelegation::Super(call) => call.args.as_slice(),
                    };
                    for &argument in delegation_arguments {
                        scoped_bodies.insert(argument);
                        visit_expression(file, argument, &constructor_visible, written, compound);
                    }
                }

                let mut constructor_visible = member_visible;
                for parameter in &class.props {
                    constructor_visible.remove(&parameter.name);
                }
                for expression in local_class_capture_expressions(class)
                    .into_iter()
                    .filter(|expression| !scoped_bodies.contains(expression))
                {
                    visit_expression(file, expression, &constructor_visible, written, compound);
                }
            }
            Stmt::CompoundAssign { target, value, .. } => {
                // This parser shape has no assignable fallback. It can only become the selected
                // in-place operator call, which reads the target but never replaces its binding.
                visit_expression(file, *target, visible, written, compound);
                visit_expression(file, *value, visible, written, compound);
            }
            _ => {
                file.any_child_stmt(statement, &mut |child| {
                    visit_expression(file, child, visible, written, compound);
                    false
                });
            }
        }
    }

    match parts {
        LoopParts::Independent(parts) => {
            for &part in parts {
                visit_expression(file, part, entry, &written, &compound);
            }
        }
        LoopParts::DoWhile { body, condition } => {
            if let Expr::Block { stmts, trailing } = file.expr(body) {
                let condition_entry =
                    visit_block(file, stmts, *trailing, entry, &written, &compound);
                visit_expression(file, condition, &condition_entry, &written, &compound);
            } else {
                visit_expression(file, body, entry, &written, &compound);
                visit_expression(file, condition, entry, &written, &compound);
            }
        }
    }
    LoopEntryWrites {
        definite: written.into_inner(),
        compound: compound.into_inner(),
    }
}

struct LoopEntryWrites {
    /// Writes that replace the binding regardless of later overload selection.
    definite: HashSet<u32>,
    /// Mutable compound targets whose selected lowering decides whether the binding is replaced.
    compound: HashMap<u32, usize>,
}

#[derive(Clone, Copy)]
enum LoopParts<'a> {
    Independent(&'a [ExprId]),
    /// A do-while body and condition share a lexical scope, unlike the two roots of other loops.
    DoWhile {
        body: ExprId,
        condition: ExprId,
    },
}

#[derive(Clone, Copy)]
struct LoopEntryBinding {
    flow_identity: u32,
    reassignable: bool,
}

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
    fn clear_narrowings_for_loop_parts(
        &mut self,
        scope: &CheckerScope<'_>,
        parts: LoopParts<'_>,
    ) -> HashMap<u32, usize> {
        let mut entry = HashMap::default();
        let mut shadowed = HashSet::default();
        scope.visit_bindings(Ns::Value, |name, binding| {
            if shadowed.insert(name.to_string()) {
                if let Some(local) = binding.value() {
                    entry.insert(
                        name.to_string(),
                        LoopEntryBinding {
                            flow_identity: local.flow_identity,
                            reassignable: local.write_ty.is_some(),
                        },
                    );
                }
            }
        });
        let written = collect_loop_entry_writes(self.file, parts, &entry);
        for (name, binding) in entry {
            let definite = written.definite.contains(&binding.flow_identity);
            if definite || written.compound.contains_key(&binding.flow_identity) {
                scope.forget_paths_rooted_at(&PathRoot::Value(binding.flow_identity));
                if definite {
                    self.forget_implied_facts(scope, binding.flow_identity);
                }
                scope.narrow_local(&name, None);
            }
        }
        written.compound
    }

    /// Drop flow proofs invalidated by a loop's back-edge writes before checking its condition.
    ///
    /// The enclosing scope must lose those proofs so the condition, body, and continuation all see
    /// the back edge. A proof established by the condition is applied afterwards and therefore
    /// remains valid on each body entry.
    pub(super) fn clear_narrowings_a_loop_invalidates(
        &mut self,
        scope: &CheckerScope<'_>,
        parts: &[ExprId],
    ) -> HashMap<u32, usize> {
        self.clear_narrowings_for_loop_parts(scope, LoopParts::Independent(parts))
    }

    pub(super) fn clear_narrowings_a_do_while_invalidates(
        &mut self,
        scope: &CheckerScope<'_>,
        body: ExprId,
        condition: ExprId,
    ) -> HashMap<u32, usize> {
        self.clear_narrowings_for_loop_parts(scope, LoopParts::DoWhile { body, condition })
    }
}
