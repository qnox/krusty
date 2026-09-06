//! Lexical signature context for parser-hoisted local classifiers.
//!
//! Hoisting gives local classes stable declaration identities, but their headers still resolve in
//! the declaration body where they were written. This module inventories that compact context for
//! Pass 1 without retaining a body or keying persistent state by transient statement IDs.

use std::collections::HashMap;

use crate::ast::{
    ClassDecl, ClassInit, Decl, DeclId, ExprId, File, FunBody, PropDecl, Stmt, StmtId,
};
use crate::types::{type_name, TypeName};

use super::class_internal;

#[derive(Clone)]
pub(super) struct EnclosingTypeParameterDeclaration {
    pub(super) declaration_start: u32,
    pub(super) names: Vec<String>,
    pub(super) bounds: Vec<(String, crate::ast::TypeRef)>,
}

fn type_parameters(
    declaration_start: u32,
    names: &[String],
    bounds: &[(String, crate::ast::TypeRef)],
) -> EnclosingTypeParameterDeclaration {
    EnclosingTypeParameterDeclaration {
        declaration_start,
        names: names.to_vec(),
        bounds: bounds.to_vec(),
    }
}

/// Every statement and expression reachable from a body, including local-class member bodies.
fn reachable_nodes(file: &File, root: ExprId) -> (Vec<StmtId>, Vec<ExprId>) {
    let mut exprs = vec![root];
    let mut stmts = Vec::new();
    let mut out_stmts = Vec::new();
    let mut out_exprs = Vec::new();
    let mut guard = 0usize;
    loop {
        guard += 1;
        if guard > 1_000_000 {
            break;
        }
        if let Some(expression) = exprs.pop() {
            out_exprs.push(expression);
            file.any_child_expr(
                expression,
                &mut |child| {
                    exprs.push(child);
                    false
                },
                &mut |child| {
                    stmts.push(child);
                    false
                },
            );
        } else if let Some(statement) = stmts.pop() {
            out_stmts.push(statement);
            file.any_child_stmt(statement, &mut |child| {
                exprs.push(child);
                false
            });
            if let Stmt::LocalClass(class) = file.stmt(statement) {
                for method in &class.methods {
                    if let FunBody::Expr(body) | FunBody::Block(body) = method.body {
                        exprs.push(body);
                    }
                }
            }
        } else {
            break;
        }
    }
    (out_stmts, out_exprs)
}

/// Record local and anonymous classifiers with the exact declaration type-parameter rungs visible
/// at their construction site. A flat descendant walk loses a local function's own formals before
/// reaching classifiers in that function body (`fun <T> local() { class C { fun id(x: T) = x } }`).
/// Keep the scope on every work item so nested local functions shadow and extend normally.
fn record_scoped_local_classifiers(
    file: &File,
    body: &FunBody,
    declarations: &[EnclosingTypeParameterDeclaration],
    result: &mut HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>>,
) {
    let (FunBody::Expr(root) | FunBody::Block(root)) = body else {
        return;
    };
    let mut expressions = vec![(*root, declarations.to_vec())];
    let mut statements = Vec::new();
    loop {
        if let Some((expression, scope)) = expressions.pop() {
            if let Some(&declaration) = file.anonymous_object_classes.get(&expression) {
                if scope
                    .iter()
                    .any(|declaration| !declaration.names.is_empty())
                {
                    result.entry(declaration).or_insert_with(|| scope.clone());
                }
                if let Decl::Class(class) = file.decl(declaration) {
                    record_class_member_local_classifiers(file, class, &scope, result);
                }
            }
            file.any_child_expr(
                expression,
                &mut |child| {
                    expressions.push((child, scope.clone()));
                    false
                },
                &mut |child| {
                    statements.push((child, scope.clone()));
                    false
                },
            );
        } else if let Some((statement, scope)) = statements.pop() {
            match file.stmt(statement) {
                Stmt::LocalClass(class) => {
                    if let Some(&declaration) = file.local_class_decls.get(&statement) {
                        if scope
                            .iter()
                            .any(|declaration| !declaration.names.is_empty())
                        {
                            result.entry(declaration).or_insert_with(|| scope.clone());
                        }
                    }
                    record_class_member_local_classifiers(file, class, &scope, result);
                }
                Stmt::LocalFun(function) => {
                    let mut nested = scope;
                    nested.push(type_parameters(
                        function.signature_span.lo,
                        &function.type_params,
                        &function.type_param_bounds,
                    ));
                    file.any_fun_expr(function, &mut |child| {
                        expressions.push((child, nested.clone()));
                        false
                    });
                }
                _ => {
                    file.any_child_stmt(statement, &mut |child| {
                        expressions.push((child, scope.clone()));
                        false
                    });
                }
            }
        } else {
            break;
        }
    }
}

/// Continue lexical type-parameter inventory through a local/anonymous classifier's own bodies.
/// Parser hoisting makes the classifier header visible from the enclosing body, but it does not
/// make member bodies children of the construction expression/statement. Without this explicit
/// scope edge, a nested classifier inside one of those members loses both the enclosing callable's
/// formals and the local class's own formals before compact signature collection.
fn record_class_member_local_classifiers(
    file: &File,
    class: &ClassDecl,
    declarations: &[EnclosingTypeParameterDeclaration],
    result: &mut HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>>,
) {
    let mut class_scope = declarations.to_vec();
    class_scope.push(type_parameters(
        class.span.lo,
        &class.type_params,
        &class.type_param_bounds,
    ));

    for method in &class.methods {
        let mut method_scope = class_scope.clone();
        method_scope.push(type_parameters(
            method.signature_span.lo,
            &method.type_params,
            &method.type_param_bounds,
        ));
        record_scoped_local_classifiers(file, &method.body, &method_scope, result);
        for default in method
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
        {
            record_scoped_local_classifiers(file, &FunBody::Expr(default), &method_scope, result);
        }
    }
    for property in &class.body_props {
        let mut property_scope = class_scope.clone();
        property_scope.push(type_parameters(
            property.span.lo,
            &property.type_params,
            &property.type_param_bounds,
        ));
        for_each_property_body(property, |body| {
            record_scoped_local_classifiers(file, &body, &property_scope, result)
        });
    }
    for step in &class.init_order {
        if let ClassInit::Block(body) = step {
            record_scoped_local_classifiers(file, &FunBody::Block(*body), &class_scope, result);
        }
    }
    for default in class.props.iter().filter_map(|parameter| parameter.default) {
        record_scoped_local_classifiers(file, &FunBody::Expr(default), &class_scope, result);
    }
    for constructor in &class.secondary_ctors {
        for expression in constructor
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
            .chain(match &constructor.delegation {
                crate::ast::CtorDelegation::This(call)
                | crate::ast::CtorDelegation::Super(call) => call.args.iter().copied(),
                crate::ast::CtorDelegation::None => [].iter().copied(),
            })
            .chain(constructor.body)
        {
            record_scoped_local_classifiers(file, &FunBody::Expr(expression), &class_scope, result);
        }
    }
}

fn for_each_property_body(property: &PropDecl, mut visit: impl FnMut(FunBody)) {
    if let Some(initializer) = property.init {
        visit(FunBody::Expr(initializer));
    }
    if let Some(delegate) = property.delegate {
        visit(FunBody::Expr(delegate));
    }
    if let Some(getter) = &property.getter {
        visit(getter.clone());
    }
    if let Some(setter) = property
        .setter
        .as_ref()
        .and_then(|setter| setter.body.as_ref())
    {
        visit(setter.clone());
    }
}

/// Type-parameter declarations visible from a member of `class`, outermost first. Only an `inner`
/// classifier inherits its containing class's parameters; a static nested classifier does not.
fn class_type_parameter_scope(
    file: &File,
    class: &ClassDecl,
) -> Vec<EnclosingTypeParameterDeclaration> {
    let mut declarations = vec![type_parameters(
        class.span.lo,
        &class.type_params,
        &class.type_param_bounds,
    )];
    let mut outer = class.inner_of.as_deref();
    let mut guard = 0;
    while let Some(owner) = outer {
        guard += 1;
        if guard > 32 {
            break;
        }
        let Some(class) = file
            .decls
            .iter()
            .find_map(|declaration| match file.decl(*declaration) {
                Decl::Class(candidate) if candidate.name == owner => Some(candidate),
                Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
            })
        else {
            break;
        };
        declarations.push(type_parameters(
            class.span.lo,
            &class.type_params,
            &class.type_param_bounds,
        ));
        outer = class.inner_of.as_deref();
    }
    declarations.reverse();
    declarations
}

pub(super) fn local_class_enclosing_tparams(
    file: &File,
) -> HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>> {
    let mut result = HashMap::new();

    let record =
        |body: &FunBody,
         declarations: &[EnclosingTypeParameterDeclaration],
         result: &mut HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>>| {
            record_scoped_local_classifiers(file, body, declarations, result);
        };

    let record_property =
        |property: &PropDecl,
         mut declarations: Vec<EnclosingTypeParameterDeclaration>,
         result: &mut HashMap<DeclId, Vec<EnclosingTypeParameterDeclaration>>| {
            declarations.push(type_parameters(
                property.span.lo,
                &property.type_params,
                &property.type_param_bounds,
            ));
            for_each_property_body(property, |body| record(&body, &declarations, result));
        };

    for &declaration in &file.decls {
        match file.decl(declaration) {
            Decl::Fun(function) => record(
                &function.body,
                &[type_parameters(
                    function.signature_span.lo,
                    &function.type_params,
                    &function.type_param_bounds,
                )],
                &mut result,
            ),
            Decl::Class(class) => {
                let class_scope = class_type_parameter_scope(file, class);
                for method in &class.methods {
                    let mut scope = class_scope.clone();
                    scope.push(type_parameters(
                        method.signature_span.lo,
                        &method.type_params,
                        &method.type_param_bounds,
                    ));
                    record(&method.body, &scope, &mut result);
                }
                for property in &class.body_props {
                    record_property(property, class_scope.clone(), &mut result);
                }
                for step in &class.init_order {
                    if let ClassInit::Block(body) = step {
                        record(&FunBody::Block(*body), &class_scope, &mut result);
                    }
                }
                for initializer in class.props.iter().filter_map(|property| property.default) {
                    record(&FunBody::Expr(initializer), &class_scope, &mut result);
                }
                for entry in &class.enum_entries {
                    for method in &entry.methods {
                        let mut scope = class_scope.clone();
                        scope.push(type_parameters(
                            method.signature_span.lo,
                            &method.type_params,
                            &method.type_param_bounds,
                        ));
                        record(&method.body, &scope, &mut result);
                    }
                    for property in &entry.props {
                        record_property(property, class_scope.clone(), &mut result);
                    }
                    for step in &entry.init_order {
                        if let ClassInit::Block(body) = step {
                            record(&FunBody::Block(*body), &class_scope, &mut result);
                        }
                    }
                }
            }
            Decl::Property(property) => record_property(property, Vec::new(), &mut result),
        }
    }

    // An `inner` classifier declared inside a local/anonymous classifier is hoisted as a sibling
    // declaration, not as a statement/expression child of the construction body. Carry the outer
    // classifier's lexical declaration rungs across that stable ownership edge, then inspect the
    // inner classifier's members for still-deeper local/anonymous declarations. Iterate because an
    // anonymous object may contain `inner First`, whose method contains another anonymous object,
    // which in turn contains `inner Second`.
    loop {
        let mut progressed = false;
        for &declaration in &file.decls {
            if result.contains_key(&declaration) {
                continue;
            }
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let Some(owner_name) = class.inner_of.as_deref() else {
                continue;
            };
            let Some((owner_declaration, owner)) =
                file.decls
                    .iter()
                    .find_map(|&candidate| match file.decl(candidate) {
                        Decl::Class(owner) if owner.name == owner_name => Some((candidate, owner)),
                        Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
                    })
            else {
                continue;
            };
            let Some(mut scope) = result.get(&owner_declaration).cloned() else {
                continue;
            };
            scope.push(type_parameters(
                owner.span.lo,
                &owner.type_params,
                &owner.type_param_bounds,
            ));
            result.insert(declaration, scope.clone());
            record_class_member_local_classifiers(file, class, &scope, &mut result);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }

    // An ordinary `inner` classifier carries its enclosing classifier's type-parameter rungs just
    // as a body-local classifier carries its enclosing callable rungs. Capture that lexical fact
    // while the source is active so production signature publication never follows `inner_of`
    // spellings back through the parser declaration arena.
    for &declaration in &file.decls {
        let Decl::Class(class) = file.decl(declaration) else {
            continue;
        };
        if class.inner_of.is_none() {
            continue;
        }
        let mut scope = class_type_parameter_scope(file, class);
        scope.pop(); // the classifier's own parameters are declared by its compact header
        result.entry(declaration).or_insert(scope);
    }
    result
}

pub(super) fn local_class_sibling_names(file: &File) -> HashMap<DeclId, Vec<(String, TypeName)>> {
    let mut result = HashMap::new();
    if file.local_class_decls.is_empty() {
        return result;
    }
    let record = |body: &FunBody, result: &mut HashMap<DeclId, Vec<(String, TypeName)>>| {
        let (FunBody::Expr(root) | FunBody::Block(root)) = body else {
            return;
        };
        let mut visible = Vec::new();
        let mut declarations = Vec::new();
        let (statements, expressions) = reachable_nodes(file, *root);
        for statement in statements {
            let Some(&declaration) = file.local_class_decls.get(&statement) else {
                continue;
            };
            let Stmt::LocalClass(class) = file.stmt(statement) else {
                continue;
            };
            let Decl::Class(hoisted) = file.decl(declaration) else {
                continue;
            };
            visible.push((
                class.name.clone(),
                type_name(&class_internal(file, &hoisted.name)),
            ));
            declarations.push(declaration);
            declarations.extend(
                file.local_class_nested
                    .get(&statement)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
        for expression in expressions {
            if let Some(&declaration) = file.anonymous_object_classes.get(&expression) {
                declarations.push(declaration);
            }
        }
        for declaration in declarations {
            result
                .entry(declaration)
                .or_default()
                .extend(visible.iter().cloned());
        }
    };

    let record_property =
        |property: &PropDecl, result: &mut HashMap<DeclId, Vec<(String, TypeName)>>| {
            for_each_property_body(property, |body| record(&body, result));
        };
    for &declaration in &file.decls {
        match file.decl(declaration) {
            Decl::Fun(function) => record(&function.body, &mut result),
            Decl::Class(class) => {
                for method in &class.methods {
                    record(&method.body, &mut result);
                }
                for property in &class.body_props {
                    record_property(property, &mut result);
                }
                for step in &class.init_order {
                    if let ClassInit::Block(body) = step {
                        record(&FunBody::Block(*body), &mut result);
                    }
                }
                for initializer in class.props.iter().filter_map(|property| property.default) {
                    record(&FunBody::Expr(initializer), &mut result);
                }
                for entry in &class.enum_entries {
                    for method in &entry.methods {
                        record(&method.body, &mut result);
                    }
                    for property in &entry.props {
                        record_property(property, &mut result);
                    }
                    for step in &entry.init_order {
                        if let ClassInit::Block(body) = step {
                            record(&FunBody::Block(*body), &mut result);
                        }
                    }
                }
            }
            Decl::Property(property) => record_property(property, &mut result),
        }
    }
    result
}
