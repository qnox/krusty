//! Lexical name-use analysis for closure and local-declaration capture planning.

use crate::fx_hash::HashSet;

use super::{Checker, ExprLowering, LambdaCapture};
use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, DeclId, Expr, ExprId, File, FunBody, Stmt, StmtId,
};
use crate::diag::Span;

/// One closure-captured write and the execution in which its assignment runs.
///
/// A local function remains a distinct execution even when it contains no materialized lambda.
/// Keeping that declaration identity beside the lambda chain prevents the enclosing function and
/// the local function from being mistaken for the same execution during smart-cast stability
/// checks.
pub(super) struct ClosureWrite {
    name: String,
    local_function: Option<StmtId>,
    lambdas: Vec<ExprId>,
    /// Assigned value for an assignment; `None` for inc/dec.
    value: Option<ExprId>,
}

/// Collect captured writes and the lambda chain that controls when each write can execute.
pub(super) fn collect_closure_reassigned(
    file: &File,
    expression: ExprId,
    out: &mut Vec<ClosureWrite>,
) {
    // The child walk takes two closures at once, so the accumulators go through `RefCell`s; the
    // lambda stack keeps its push/pop discipline because every borrow ends inside the recursive
    // call that took it.
    let writes = std::cell::RefCell::new(std::mem::take(out));
    let lambdas = std::cell::RefCell::new(Vec::new());
    let local_functions = std::cell::RefCell::new(Vec::new());

    fn visit_expression(
        file: &File,
        expression: ExprId,
        lambdas: &std::cell::RefCell<Vec<ExprId>>,
        local_functions: &std::cell::RefCell<Vec<StmtId>>,
        writes: &std::cell::RefCell<Vec<ClosureWrite>>,
    ) {
        let is_lambda = matches!(file.expr(expression), Expr::Lambda { .. });
        if is_lambda {
            lambdas.borrow_mut().push(expression);
        }
        if !lambdas.borrow().is_empty() || !local_functions.borrow().is_empty() {
            if let Expr::IncDec { target, .. } = file.expr(expression) {
                if let Expr::Name(name) = file.expr(*target) {
                    writes.borrow_mut().push(ClosureWrite {
                        name: name.clone(),
                        local_function: local_functions.borrow().last().copied(),
                        lambdas: lambdas.borrow().clone(),
                        value: None,
                    });
                }
            }
        }
        file.any_child_expr(
            expression,
            &mut |child| {
                visit_expression(file, child, lambdas, local_functions, writes);
                false
            },
            &mut |statement| {
                visit_statement(file, statement, lambdas, local_functions, writes);
                false
            },
        );
        if is_lambda {
            lambdas.borrow_mut().pop();
        }
    }

    fn visit_statement(
        file: &File,
        statement: StmtId,
        lambdas: &std::cell::RefCell<Vec<ExprId>>,
        local_functions: &std::cell::RefCell<Vec<StmtId>>,
        writes: &std::cell::RefCell<Vec<ClosureWrite>>,
    ) {
        match file.stmt(statement) {
            Stmt::LocalFun(function) => {
                local_functions.borrow_mut().push(statement);
                for default in function
                    .params
                    .iter()
                    .filter_map(|parameter| parameter.default)
                {
                    visit_expression(file, default, lambdas, local_functions, writes);
                }
                if let FunBody::Expr(body) | FunBody::Block(body) = &function.body {
                    visit_expression(file, *body, lambdas, local_functions, writes);
                }
                local_functions.borrow_mut().pop();
                return;
            }
            Stmt::Assign { name, value }
                if !lambdas.borrow().is_empty() || !local_functions.borrow().is_empty() =>
            {
                writes.borrow_mut().push(ClosureWrite {
                    name: name.clone(),
                    local_function: local_functions.borrow().last().copied(),
                    lambdas: lambdas.borrow().clone(),
                    value: Some(*value),
                });
            }
            Stmt::IncDec { name, .. }
                if !lambdas.borrow().is_empty() || !local_functions.borrow().is_empty() =>
            {
                writes.borrow_mut().push(ClosureWrite {
                    name: name.clone(),
                    local_function: local_functions.borrow().last().copied(),
                    lambdas: lambdas.borrow().clone(),
                    value: None,
                });
            }
            _ => {}
        }
        file.any_child_stmt(statement, &mut |child| {
            visit_expression(file, child, lambdas, local_functions, writes);
            false
        });
    }

    visit_expression(file, expression, &lambdas, &local_functions, &writes);
    *out = writes.into_inner();
}

impl<'a> Checker<'a> {
    /// Whether a captured write that exists by `site` can mutate `name` outside the execution
    /// currently being checked. Writes in the same local-function/materialized-lambda execution
    /// are ordinary straight-line writes: the assignment itself replaces the flow fact, and a
    /// later read may use that exact assigned type. A sibling or nested escaping callable can run
    /// between proof and use and therefore still makes the variable unstable. Inline-spliced
    /// lambdas do not add an execution boundary on either side of the comparison.
    pub(super) fn closure_reassigned_before(&self, name: &str, site: Span) -> bool {
        self.fn_closure_reassigned.iter().any(|write| {
            self.capturing_closure_write_exists_before(write, name, site)
                && !self.same_materialized_execution(write)
        })
    }

    /// Whether a materialized capturing closure containing a write exists by `site`, irrespective
    /// of which closure body the checker is currently visiting. This is the enclosing scope's
    /// invalidation question; [`Self::closure_reassigned_before`] is the current-execution stability
    /// question.
    pub(super) fn capturing_closure_reassigned_before(&self, name: &str, site: Span) -> bool {
        self.fn_closure_reassigned
            .iter()
            .any(|write| self.capturing_closure_write_exists_before(write, name, site))
    }

    fn capturing_closure_write_exists_before(
        &self,
        write: &ClosureWrite,
        name: &str,
        site: Span,
    ) -> bool {
        write.name == name
            && (write
                .local_function
                .is_some_and(|function| self.file.stmt_spans[function.0 as usize].lo <= site.lo)
                || write.lambdas.iter().any(|&lambda| {
                    self.span(lambda).lo <= site.lo && !self.lambda_is_inline_spliced(lambda)
                }))
    }

    fn same_materialized_execution(&self, write: &ClosureWrite) -> bool {
        write.local_function == self.local_function_execution
            && write
                .lambdas
                .iter()
                .copied()
                .filter(|&lambda| !self.lambda_is_inline_spliced(lambda))
                .eq(self
                    .lambda_returns
                    .active_chain()
                    .iter()
                    .copied()
                    .filter(|&lambda| !self.lambda_is_inline_spliced(lambda)))
    }

    /// Whether every relevant capturing-lambda write stores the null literal.
    pub(super) fn closure_writes_only_null(&self, name: &str, site: Span) -> bool {
        self.fn_closure_reassigned
            .iter()
            .filter(|write| {
                self.capturing_closure_write_exists_before(write, name, site)
                    && !self.same_materialized_execution(write)
            })
            .all(|write| {
                write
                    .value
                    .is_some_and(|value| matches!(self.file.expr(value), Expr::NullLit))
            })
    }

    /// The checker's recorded capture decision for a lambda expression (set when the enclosing
    /// call is checked against an inline callee).
    fn lambda_is_inline_spliced(&self, expression: ExprId) -> bool {
        matches!(
            self.expr_lowers.get(&expression),
            Some(ExprLowering::Lambda(info)) if info.capture == LambdaCapture::InlineSplice
        )
    }
}

/// Hoisted statement-position local classes lexically contained by an expression subtree.
///
/// Class member bodies deliberately are not traversed here: capture discovery has already folded
/// every value needed by a nested/member body into the containing class's exact capture list. The
/// enclosing callable only needs to carry those constructor inputs to the class declaration site.
pub(super) fn local_class_declarations(file: &File, expression: ExprId) -> Vec<DeclId> {
    fn visit_expression(file: &File, expression: ExprId, classes: &mut Vec<DeclId>) {
        let mut expressions = Vec::new();
        let mut statements = Vec::new();
        file.any_child_expr(
            expression,
            &mut |child| {
                expressions.push(child);
                false
            },
            &mut |statement| {
                statements.push(statement);
                false
            },
        );
        for child in expressions {
            visit_expression(file, child, classes);
        }
        for statement in statements {
            visit_statement(file, statement, classes);
        }
    }

    fn visit_statement(file: &File, statement: StmtId, classes: &mut Vec<DeclId>) {
        if matches!(file.stmt(statement), Stmt::LocalClass(_)) {
            if let Some(&declaration) = file.local_class_decls.get(&statement) {
                classes.push(declaration);
            }
            return;
        }
        file.any_child_stmt(statement, &mut |child| {
            visit_expression(file, child, classes);
            false
        });
    }

    let mut classes = Vec::new();
    visit_expression(file, expression, &mut classes);
    classes
}

/// Every name from `outer` read by the expression subtree, in stable spelling order.
pub(super) fn used_names(file: &File, expression: ExprId, outer: &HashSet<String>) -> Vec<String> {
    let mut names = outer.iter().collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .filter(|name| {
            let one = std::iter::once((*name).clone()).collect();
            local_fun_body_uses_any(file, expression, &one)
        })
        .cloned()
        .collect()
}

/// Whether a property initializer or delegate reads an enclosing value with the property's own
/// spelling. Kotlin keeps the declaration being initialized out of that value lookup: in
/// `fun f(x: Int) = object { val x: Int = x }`, the right-hand `x` is the lexical parameter.
/// A function local stays visible through a same-named accessor too; capture selection keeps
/// that local beside the member. `this.x` is the property.
pub(super) fn own_property_initializer_uses_outer_name(
    file: &File,
    declaration: DeclId,
    name: &str,
) -> bool {
    let Decl::Class(class) = file.decl(declaration) else {
        return false;
    };
    class.body_props.iter().any(|property| {
        property.name == name
            && property
                .init
                .into_iter()
                .chain(property.delegate)
                .any(|expression| file.expr_uses_name_deep(expression, name))
    })
}

/// Whether an expression subtree references any active enclosing value.
///
/// The active set is narrowed at each lexical declaration. Nested local functions are traversed:
/// their direct capture must also be carried transitively by every enclosing callable body.
pub(super) fn local_fun_body_uses_any(
    file: &File,
    expression: ExprId,
    outer: &HashSet<String>,
) -> bool {
    fn class_uses(file: &File, class: &ClassDecl, active: &HashSet<String>) -> bool {
        // A name this class introduces hides an enclosing value only when the enclosing scope
        // does not already have that value. A function local keeps its spelling through a
        // same-named property (`fun f(x: Int) { class C { val x = 2; fun g() = x } }`). Nested
        // local declarations are still traversed below, so a value used only by a local function
        // inside a nested local class is carried through every enclosing capture boundary.
        let mut class_active = active.clone();
        for name in class
            .props
            .iter()
            .map(|property| &property.name)
            .chain(class.body_props.iter().map(|property| &property.name))
            .chain(class.methods.iter().map(|method| &method.name))
        {
            if !active.contains(name.as_str()) {
                class_active.remove(name);
            }
        }
        // Constructor parameters enter scope only after their own defaults. An outer `x` is still
        // the binding in `class C(x: T = run { use(x) })`, while a later parameter default and the
        // completed class header see the newly declared constructor parameter.
        let mut constructor_active = active.clone();
        for parameter in &class.context_params {
            constructor_active.remove(&parameter.name);
        }
        for parameter in &class.props {
            if parameter
                .default
                .is_some_and(|default| expression_uses(file, default, &constructor_active))
            {
                return true;
            }
            constructor_active.remove(&parameter.name);
        }
        if class
            .base_args
            .iter()
            .copied()
            .any(|expression| expression_uses(file, expression, &constructor_active))
        {
            return true;
        }
        for step in &class.init_order {
            if let ClassInit::Block(body) = step {
                if expression_uses(file, *body, &class_active) {
                    return true;
                }
            }
        }
        for property in &class.body_props {
            let mut initializer_active = class_active.clone();
            if active.contains(&property.name) {
                initializer_active.insert(property.name.clone());
            }
            if property
                .init
                .into_iter()
                .chain(property.delegate)
                .any(|expression| expression_uses(file, expression, &initializer_active))
            {
                return true;
            }
            for body in property.getter.iter().chain(
                property
                    .setter
                    .iter()
                    .filter_map(|setter| setter.body.as_ref()),
            ) {
                let (FunBody::Expr(body) | FunBody::Block(body)) = body else {
                    continue;
                };
                if expression_uses(file, *body, &class_active) {
                    return true;
                }
            }
        }
        for method in &class.methods {
            let mut method_active = class_active.clone();
            for parameter in &method.params {
                if parameter
                    .default
                    .is_some_and(|default| expression_uses(file, default, &method_active))
                {
                    return true;
                }
                method_active.remove(&parameter.name);
            }
            if let FunBody::Expr(body) | FunBody::Block(body) = method.body {
                if expression_uses(file, body, &method_active) {
                    return true;
                }
            }
        }
        for constructor in &class.secondary_ctors {
            let mut constructor_active = class_active.clone();
            for parameter in &constructor.params {
                if parameter
                    .default
                    .is_some_and(|default| expression_uses(file, default, &constructor_active))
                {
                    return true;
                }
                constructor_active.remove(&parameter.name);
            }
            let arguments = match &constructor.delegation {
                CtorDelegation::None => &[][..],
                CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
            };
            if constructor
                .body
                .into_iter()
                .chain(arguments.iter().copied())
                .any(|expression| expression_uses(file, expression, &constructor_active))
            {
                return true;
            }
        }
        false
    }

    fn expression_uses(file: &File, expression: ExprId, active: &HashSet<String>) -> bool {
        match file.expr(expression) {
            Expr::Name(name) => active.contains(name),
            Expr::Block { stmts, trailing } => {
                let mut active = active.clone();
                for &statement in stmts {
                    if statement_uses(file, statement, &mut active) {
                        return true;
                    }
                }
                trailing.is_some_and(|trailing| expression_uses(file, trailing, &active))
            }
            Expr::Lambda { params, body } => {
                let mut active = active.clone();
                for parameter in params {
                    active.remove(parameter);
                }
                if params.is_empty() {
                    active.remove("it");
                }
                expression_uses(file, *body, &active)
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                if expression_uses(file, *body, active) {
                    return true;
                }
                for catch in catches {
                    let mut catch_active = active.clone();
                    catch_active.remove(&catch.name);
                    if expression_uses(file, catch.body, &catch_active) {
                        return true;
                    }
                }
                finally.is_some_and(|finally| expression_uses(file, finally, active))
            }
            _ => file.any_child_expr(
                expression,
                &mut |child| expression_uses(file, child, active),
                &mut |statement| {
                    let mut active = active.clone();
                    statement_uses(file, statement, &mut active)
                },
            ),
        }
    }

    fn statement_uses(file: &File, statement: StmtId, active: &mut HashSet<String>) -> bool {
        match file.stmt(statement) {
            Stmt::IncDec { name, .. } => active.contains(name),
            // Assignment targets are statement spellings rather than `Expr::Name` children.
            Stmt::Assign { name, value } => {
                active.contains(name) || expression_uses(file, *value, active)
            }
            Stmt::Local { name, init, .. } => {
                let used = expression_uses(file, *init, active);
                active.remove(name);
                used
            }
            Stmt::LocalDelegate { name, delegate, .. } => {
                let used = expression_uses(file, *delegate, active);
                active.remove(name);
                used
            }
            Stmt::Destructure { entries, init } => {
                let used = expression_uses(file, *init, active);
                for entry in entries.iter().filter(|entry| !entry.ignored) {
                    active.remove(&entry.name);
                }
                used
            }
            Stmt::For {
                name, range, body, ..
            } => {
                if expression_uses(file, range.start, active)
                    || expression_uses(file, range.end, active)
                {
                    return true;
                }
                let mut body_active = active.clone();
                body_active.remove(name);
                expression_uses(file, *body, &body_active)
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                if expression_uses(file, *iterable, active) {
                    return true;
                }
                let mut body_active = active.clone();
                body_active.remove(name);
                expression_uses(file, *body, &body_active)
            }
            Stmt::LocalFun(function) => {
                // Defaults see only preceding parameters.
                let mut defaults_active = active.clone();
                for parameter in &function.params {
                    if parameter
                        .default
                        .is_some_and(|default| expression_uses(file, default, &defaults_active))
                    {
                        return true;
                    }
                    defaults_active.remove(&parameter.name);
                }

                let mut body_active = active.clone();
                body_active.remove(&function.name);
                for parameter in &function.params {
                    body_active.remove(&parameter.name);
                }
                match function.body {
                    FunBody::Expr(body) | FunBody::Block(body) => {
                        expression_uses(file, body, &body_active)
                    }
                    FunBody::None => false,
                }
            }
            Stmt::LocalClass(class) => class_uses(file, class, active),
            _ => file.any_child_stmt(statement, &mut |child| expression_uses(file, child, active)),
        }
    }

    expression_uses(file, expression, outer)
}
