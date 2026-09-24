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

/// Bounded body-containment facts retained after one Pass-1 parser arena is released.
#[derive(Default, Clone)]
pub(crate) struct PassOneLocalClassContext {
    pub(super) parser_declaration_identities: crate::fir::ParserDeclarationIdentities,
    pub(super) parser_classifier_identities: HashMap<DeclId, TypeName>,
    pub(super) enclosing_type_parameters:
        HashMap<crate::fir::DeclarationId, Vec<EnclosingTypeParameterDeclaration>>,
    pub(super) sibling_classifiers: HashMap<crate::fir::DeclarationId, Vec<(String, TypeName)>>,
    pub(super) anonymous_owners: HashMap<crate::fir::DeclarationId, crate::fir::DeclarationId>,
    pub(super) anonymous_declarations: std::collections::HashSet<crate::fir::DeclarationId>,
}

impl PassOneLocalClassContext {
    pub(crate) fn parser_classifier_identities(&self) -> &HashMap<DeclId, TypeName> {
        &self.parser_classifier_identities
    }

    /// Local classifiers visible from `declaration`, already paired with the stable identities
    /// assigned while the source parser arena was live.
    pub(in crate::resolve) fn sibling_classifiers(
        &self,
        _headers: &crate::fir::StreamedHeaderModule,
        declaration: crate::fir::DeclarationId,
    ) -> Vec<(String, TypeName)> {
        self.sibling_classifiers
            .get(&declaration)
            .cloned()
            .unwrap_or_default()
    }
}

pub(crate) fn pass_one_local_class_context(
    file: &File,
    stubs: &[crate::fir::DeclarationStub],
    stable_by_transient: &crate::fir::ParserDeclarationIdentities,
) -> PassOneLocalClassContext {
    let transient_tparams = local_class_enclosing_tparams(file);
    let parser_classifier_identities =
        stable_local_classifier_identities(file, stubs, stable_by_transient);
    for declaration in file.local_class_decls.values() {
        assert!(
            parser_classifier_identities.contains_key(declaration),
            "every parser-hoisted local classifier must bind a stable semantic identity"
        );
    }
    let stable_siblings =
        stable_local_class_sibling_names(file, stable_by_transient, &parser_classifier_identities);
    let transient_anonymous = super::anonymous_lexical_class_scope(file);
    crate::trace_compiler!(
        "fir",
        "Pass 1 local classifier context stable={} type-parameter-scopes={:?}",
        stable_by_transient.len(),
        transient_tparams
            .iter()
            .map(|(declaration, parameters)| {
                let name = match file.decl(*declaration) {
                    Decl::Class(class) => class.name.as_str(),
                    Decl::Fun(_) | Decl::Property(_) => "<non-classifier>",
                };
                (
                    name,
                    stable_by_transient.get(declaration),
                    parameters
                        .iter()
                        .flat_map(|parameter| parameter.names.iter())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>(),
    );
    PassOneLocalClassContext {
        parser_declaration_identities: stable_by_transient.clone(),
        parser_classifier_identities,
        enclosing_type_parameters: transient_tparams
            .into_iter()
            .filter_map(|(declaration, parameters)| {
                stable_by_transient
                    .get(&declaration)
                    .copied()
                    .map(|stable| (stable, parameters))
            })
            .collect(),
        sibling_classifiers: stable_siblings,
        anonymous_owners: transient_anonymous
            .owners
            .into_iter()
            .filter_map(|(declaration, owner)| {
                Some((
                    *stable_by_transient.get(&declaration)?,
                    *stable_by_transient.get(&owner)?,
                ))
            })
            .collect(),
        anonymous_declarations: transient_anonymous
            .declarations
            .into_iter()
            .filter_map(|declaration| stable_by_transient.get(&declaration).copied())
            .collect(),
    }
}

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

/// Local classifiers visible from each local or anonymous classifier's lexical body, as their
/// source spelling paired with the hoisted declaration. Callers choose the identity: the legacy
/// path spells the hoisted name, while compact signature collection maps the declaration to its
/// stable module identity.
pub(super) fn local_class_sibling_declarations(
    file: &File,
) -> HashMap<DeclId, Vec<(String, DeclId)>> {
    let mut result = HashMap::new();
    if file.local_class_decls.is_empty() {
        return result;
    }
    let record = |body: &FunBody, result: &mut HashMap<DeclId, Vec<(String, DeclId)>>| {
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
            let Decl::Class(_) = file.decl(declaration) else {
                continue;
            };
            visible.push((class.name.clone(), declaration));
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
        |property: &PropDecl, result: &mut HashMap<DeclId, Vec<(String, DeclId)>>| {
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

pub(super) fn local_class_sibling_names(file: &File) -> HashMap<DeclId, Vec<(String, TypeName)>> {
    local_class_sibling_declarations(file)
        .into_iter()
        .map(|(owner, siblings)| {
            let siblings = siblings
                .into_iter()
                .filter_map(|(name, declaration)| {
                    let Decl::Class(class) = file.decl(declaration) else {
                        return None;
                    };
                    Some((name, type_name(&class_internal(file, &class.name))))
                })
                .collect();
            (owner, siblings)
        })
        .collect()
}

pub(super) fn stable_local_classifier_identities(
    file: &File,
    stubs: &[crate::fir::DeclarationStub],
    stable_by_transient: &HashMap<DeclId, crate::fir::DeclarationId>,
) -> HashMap<DeclId, TypeName> {
    let package = type_name(
        &file
            .package
            .as_deref()
            .unwrap_or_default()
            .replace('.', "/"),
    );
    let classifiers = stable_by_transient
        .iter()
        .filter_map(|(&parser, &stable)| {
            stubs
                .iter()
                .find(|stub| {
                    stub.id == stable && stub.kind == crate::fir::DeclarationKind::Classifier
                })
                .map(|stub| (parser, stable, stub.flags))
        })
        .collect::<Vec<_>>();
    let local_member_owners = classifiers
        .iter()
        .filter(|(_, _, flags)| flags.has(crate::fir::DeclarationFlags::CLASSIFIER_MEMBER))
        .filter_map(|(member, _, _)| {
            let member = *member;
            let Decl::Class(member_class) = file.decl(member) else {
                return None;
            };
            let owner = classifiers
                .iter()
                .map(|(parser, _, _)| *parser)
                .filter(|candidate| *candidate != member)
                .filter_map(|candidate| {
                    let Decl::Class(candidate_class) = file.decl(candidate) else {
                        return None;
                    };
                    (candidate_class.span.lo <= member_class.span.lo
                        && member_class.span.hi <= candidate_class.span.hi)
                        .then_some((candidate_class.span.hi - candidate_class.span.lo, candidate))
                })
                .min_by_key(|(length, _)| *length)
                .map(|(_, owner)| owner)?;
            Some((member, owner))
        })
        .collect::<HashMap<_, _>>();
    let mut identities = classifiers
        .iter()
        .filter(|(_, _, flags)| !flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
        .filter_map(|(parser, _, _)| {
            let Decl::Class(class) = file.decl(*parser) else {
                return None;
            };
            Some((*parser, type_name(&class_internal(file, &class.name))))
        })
        .collect::<HashMap<_, _>>();
    let mut pending = classifiers
        .into_iter()
        .filter(|(_, _, flags)| flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS))
        .collect::<Vec<_>>();
    loop {
        let before = pending.len();
        pending.retain(|(parser, stable, _)| {
            let identity = if let Some(owner) = local_member_owners.get(parser) {
                identities.get(owner).and_then(|owner| {
                    let segment = file
                        .hoisted_classifier_source_names
                        .get(parser)?
                        .rsplit('.')
                        .next()?;
                    Some(crate::types::type_name_nested_child(*owner, segment))
                })
            } else {
                Some(crate::fir::classifier_identity(package, *stable))
            };
            let Some(identity) = identity else {
                return true;
            };
            assert!(
                identities.insert(*parser, identity).is_none(),
                "one parser classifier binds one semantic identity"
            );
            false
        });
        if pending.is_empty() || pending.len() == before {
            break;
        }
    }
    assert!(
        pending.is_empty(),
        "every local classifier member must bind its stable semantic owner"
    );
    identities
}

pub(super) fn stable_local_class_sibling_names(
    file: &File,
    stable_by_transient: &HashMap<DeclId, crate::fir::DeclarationId>,
    identities: &HashMap<DeclId, TypeName>,
) -> HashMap<crate::fir::DeclarationId, Vec<(String, TypeName)>> {
    local_class_sibling_declarations(file)
        .into_iter()
        .filter_map(|(owner, siblings)| {
            let owner = stable_by_transient.get(&owner).copied()?;
            let siblings = siblings
                .into_iter()
                .filter_map(|(name, sibling)| {
                    identities
                        .get(&sibling)
                        .copied()
                        .map(|identity| (name, identity))
                })
                .collect();
            Some((owner, siblings))
        })
        .collect()
}
