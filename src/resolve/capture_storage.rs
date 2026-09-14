//! How a captured binding is REPRESENTED, and the write analysis that decides it.
//!
//! A closure that only reads a binding can be handed a copy of its value. One that writes it — or
//! that shares it with something else that writes it — cannot: every closure and the enclosing
//! frame have to see the same storage, so the binding becomes one mutable cell and every capture
//! of it carries that cell. Kotlin makes this invisible, and getting it wrong is invisible too
//! until the program runs: a copy answers with a stale value, and a cell handed where a value is
//! expected is a pointer read as an `Int`.
//!
//! Deciding it is one rule over four facts about the binding ([`CapturedBinding`]) plus the
//! question the rest of this module answers: does anything in the body being checked, or in any
//! anonymous body nested inside it, WRITE this name? That question is asked of the AST rather than
//! of resolved types, because the answer has to be known before the capture list exists.
//!
//! The inventory of expressions a class body can evaluate ([`class_capture_expressions`]) is shared
//! by read and write discovery on purpose: a body form left out of it records immutable or missing
//! storage, and checked FIR is then unable to represent the source capture at all.

use super::*;

/// What the checker knows about a binding when it decides how a capture of it is represented.
///
/// Named fields rather than positional arguments: three of the four are booleans, and swapping two
/// of them is a silent miscompile rather than a type error.
#[derive(Clone, Copy, Debug)]
pub(super) struct CapturedBinding {
    /// A local DELEGATED property. It reads and writes through an immutable storage object, so a
    /// capture of it captures that object and never a cell, however mutable the property is.
    pub(super) delegated: bool,
    /// Declared `var`. A `val` can always be copied.
    pub(super) mutable: bool,
    /// The binding is ALREADY one shared cell: it is a capture field of an enclosing local or
    /// anonymous classifier, declared as a cell when that classifier's captures were planned.
    pub(super) already_shared: bool,
    /// Something in the body being checked writes the name.
    pub(super) written_here: bool,
}

impl CapturedBinding {
    /// Is a capture of this binding represented by one shared cell rather than copied by value?
    ///
    /// `already_shared` is the part a body cannot work out for itself, and leaving it out is the
    /// defect this contract exists to prevent: the write that made the binding shared happened in
    /// an ENCLOSING callable, so the reassignment sets of the body being checked are empty and say
    /// nothing about it. A capture that is already a cell stays one however many callables it
    /// crosses.
    pub(super) fn is_shared_cell(self) -> bool {
        !self.delegated && self.mutable && (self.already_shared || self.written_here)
    }
}

/// Value-namespace declarations owned by an anonymous body.
///
/// Member function names are deliberately absent. An enclosing callable value has the earlier
/// lexical-value rung in call syntax, so `encode(value)` inside an anonymous member also named
/// `encode` must capture and invoke the enclosing value.
pub(super) fn anonymous_body_bound_value_names(
    file: &File,
    declaration: DeclId,
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let Decl::Class(class) = file.decl(declaration) else {
        return names;
    };
    names.extend(class.props.iter().map(|property| property.name.clone()));
    names.extend(
        class
            .body_props
            .iter()
            .map(|property| property.name.clone()),
    );
    for method in &class.methods {
        names.extend(method.params.iter().map(|parameter| parameter.name.clone()));
    }
    names
}

pub(super) fn expression_writes_name(file: &File, expression: ExprId, name: &str) -> bool {
    // A postfix/prefix increment may be the trailing expression of a lambda. In that shape the
    // parent block exposes the `Expr::IncDec` itself rather than a `Stmt::IncDec`, so inspecting only
    // child statements loses the write and incorrectly freezes an anonymous-class capture as a
    // synthetic `val` field. The target identity is still lexical here; member/index increments use
    // their own target forms and must not make an unrelated same-spelled local mutable.
    if matches!(
        file.expr(expression),
        Expr::IncDec { target, .. }
            if matches!(file.expr(*target), Expr::Name(target_name) if target_name == name)
    ) {
        return true;
    }
    let mut child_expressions = Vec::new();
    let mut child_statements = Vec::new();
    file.any_child_expr(
        expression,
        &mut |child| {
            child_expressions.push(child);
            false
        },
        &mut |statement| {
            child_statements.push(statement);
            false
        },
    );
    if child_statements.iter().any(|statement| {
        matches!(
            file.stmt(*statement),
            Stmt::Assign { name: target, .. } | Stmt::IncDec { name: target, .. }
                if target == name
        )
    }) {
        return true;
    }
    child_expressions
        .into_iter()
        .any(|child| expression_writes_name(file, child, name))
        || child_statements.into_iter().any(|statement| {
            let mut expressions = Vec::new();
            file.any_child_stmt(statement, &mut |child| {
                expressions.push(child);
                false
            });
            expressions
                .into_iter()
                .any(|child| expression_writes_name(file, child, name))
        })
}

/// Every expression a class body can evaluate while observing an enclosing lexical capture. Read
/// and write discovery must share this inventory; omitting a body form records immutable or missing
/// storage and leaves checked FIR unable to represent the source capture.
pub(super) fn class_capture_expressions(class: &ClassDecl) -> Vec<ExprId> {
    let mut expressions = class
        .methods
        .iter()
        .filter_map(|method| fun_body_expr(&method.body))
        .chain(class.body_props.iter().filter_map(|property| property.init))
        .chain(
            class
                .body_props
                .iter()
                .filter_map(|property| property.delegate),
        )
        .chain(
            class
                .body_props
                .iter()
                .filter_map(|property| property.getter.as_ref().and_then(fun_body_expr)),
        )
        .chain(class.body_props.iter().filter_map(|property| {
            property
                .setter
                .as_ref()
                .and_then(|setter| setter.body.as_ref())
                .and_then(fun_body_expr)
        }))
        .chain(class.base_args.iter().copied())
        .chain(class.props.iter().filter_map(|property| property.default))
        .chain(class.init_order.iter().filter_map(|step| match step {
            ClassInit::Block(body) => Some(*body),
            ClassInit::PropInit(_) => None,
        }))
        .collect::<Vec<_>>();
    for constructor in &class.secondary_ctors {
        expressions.extend(constructor.body);
        expressions.extend(
            constructor
                .params
                .iter()
                .filter_map(|parameter| parameter.default),
        );
        expressions.extend(match &constructor.delegation {
            CtorDelegation::None => &[][..],
            CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
        });
    }
    expressions
}

/// Statement-position local classifiers evaluate interface-delegate values in their constructor
/// context, so those values participate in outer capture and mutation analysis. Anonymous objects
/// evaluate the same syntax at their lexical construction expression and carry it as explicit FIR;
/// their ordinary body-capture inventory deliberately uses [`class_capture_expressions`] instead.
pub(super) fn local_class_capture_expressions(class: &ClassDecl) -> Vec<ExprId> {
    let mut expressions = class_capture_expressions(class);
    expressions.extend(
        class
            .interface_delegations
            .iter()
            .map(|delegation| delegation.value),
    );
    expressions
}

pub(super) fn anonymous_body_expressions(file: &File, declaration: DeclId) -> Vec<ExprId> {
    let Decl::Class(class) = file.decl(declaration) else {
        return Vec::new();
    };
    class_capture_expressions(class)
}

pub(super) fn anonymous_body_writes_name(file: &File, declaration: DeclId, name: &str) -> bool {
    anonymous_body_expressions(file, declaration)
        .into_iter()
        .any(|expression| expression_writes_name(file, expression, name))
}

pub(super) fn anonymous_descendants(
    declaration: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
) -> impl Iterator<Item = DeclId> + '_ {
    std::iter::once(declaration).chain(lexical_scope.owners.keys().copied().filter(
        move |candidate| {
            *candidate != declaration
                && lexical_scope
                    .declaration_chain(*candidate)
                    .into_iter()
                    .skip(1)
                    .any(|owner| owner == declaration)
        },
    ))
}

pub(super) fn anonymous_descendant_writes_name(
    file: &File,
    declaration: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    name: &str,
) -> bool {
    anonymous_descendants(declaration, lexical_scope).any(|candidate| {
        (!anonymous_body_bound_value_names(file, candidate).contains(name)
            || capture_analysis::own_property_initializer_uses_outer_name(file, candidate, name))
            && (anonymous_body_writes_name(file, candidate, name)
                || candidate != declaration
                    && matches!(file.decl(candidate), Decl::Class(class) if class
                        .interface_delegations
                        .iter()
                        .any(|delegation| expression_writes_name(file, delegation.value, name))))
    })
}
