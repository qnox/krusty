//! Lexical name-use analysis for closure and local-declaration capture planning.

use std::collections::HashSet;

use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, DeclId, Expr, ExprId, File, FunBody, Stmt, StmtId,
};

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
/// Accessors and other member bodies still see the property normally.
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
        // Class members and constructor parameters shadow enclosing values throughout the class
        // body. Nested local declarations are still traversed below, so a value used only by a
        // local function inside a nested local class is carried through every enclosing capture
        // boundary.
        let mut class_active = active.clone();
        for name in class
            .props
            .iter()
            .map(|property| &property.name)
            .chain(class.body_props.iter().map(|property| &property.name))
            .chain(class.methods.iter().map(|method| &method.name))
        {
            class_active.remove(name);
        }
        if class
            .base_args
            .iter()
            .copied()
            .chain(class.props.iter().filter_map(|property| property.default))
            .any(|expression| expression_uses(file, expression, &class_active))
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
                constructor_active.remove(&parameter.name);
            }
            let arguments = match &constructor.delegation {
                CtorDelegation::None => &[][..],
                CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
            };
            if constructor
                .params
                .iter()
                .filter_map(|parameter| parameter.default)
                .chain(arguments.iter().copied())
                .chain(constructor.body)
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
