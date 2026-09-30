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
    // A non-constant super-constructor argument is evaluated at the construction site and forwarded.
    // Only the bare name that stays in the anonymous constructor (after a cast or not-null assertion)
    // is a use of this class. Names inside the forwarded expression are uses of the caller.
    let forwarded = class
        .base_args
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut expressions = class_capture_expressions(class)
        .into_iter()
        .filter(|expression| !forwarded.contains(expression))
        .collect::<Vec<_>>();
    for argument in &class.base_args {
        if let Some(observed) = anonymous_super_argument_constructor_use(file, *argument) {
            expressions.push(observed);
        }
    }
    expressions
}

/// The subexpression of an anonymous super-constructor argument that the anonymous constructor
/// itself still evaluates. A bare name stays. A larger expression is forwarded from the
/// construction site, so names inside it are not uses of the anonymous class.
pub(super) fn anonymous_super_argument_constructor_use(
    file: &File,
    mut expression: ExprId,
) -> Option<ExprId> {
    loop {
        match file.expr(expression) {
            Expr::As { operand, .. } | Expr::NotNull { operand } => expression = *operand,
            Expr::Name(_) => return Some(expression),
            _ => return None,
        }
    }
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

fn expression_has_member_call_named(file: &File, expression: ExprId, name: &str) -> bool {
    // A nested lambda is another runtime closure, but a value from outside the anonymous class
    // still has to cross the class boundary before that closure can capture it.
    let matches = match file.expr(expression) {
        Expr::SafeCall { name: member, .. } => member == name,
        Expr::Call { callee, .. } => {
            matches!(file.expr(*callee), Expr::Member { name: member, .. } if member == name)
        }
        _ => false,
    };
    if matches {
        return true;
    }
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
    expressions
        .into_iter()
        .any(|child| expression_has_member_call_named(file, child, name))
        || statements.into_iter().any(|statement| {
            let mut children = Vec::new();
            file.any_child_stmt(statement, &mut |child| {
                children.push(child);
                false
            });
            children
                .into_iter()
                .any(|child| expression_has_member_call_named(file, child, name))
        })
}

fn anonymous_body_uses_name(file: &File, declaration: DeclId, name: &str, ty: Ty) -> bool {
    let expressions = anonymous_body_expressions(file, declaration);
    expressions
        .iter()
        .any(|expression| file.expr_uses_name_deep(*expression, name))
        || matches!(ty, Ty::Fun(signature) if signature.has_receiver)
            && expressions
                .into_iter()
                .any(|expression| expression_has_member_call_named(file, expression, name))
}

pub(super) fn anonymous_descendant_uses_name(
    file: &File,
    declaration: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    name: &str,
    ty: Ty,
    function_local: bool,
) -> bool {
    anonymous_descendants(declaration, lexical_scope).any(|candidate| {
        enclosing_value_visible_beside_member(file, candidate, name, function_local)
            && (anonymous_body_uses_name(file, candidate, name, ty)
                || candidate != declaration
                    && matches!(file.decl(candidate), Decl::Class(class) if class
                        .interface_delegations
                        .iter()
                        .any(|delegation| file.expr_uses_name_deep(delegation.value, name))))
    })
}

/// Whether `name` still denotes an enclosing value inside `declaration`.
///
/// A same-named member hides a top-level or class property, except when the member's own
/// initializer reads that value (`val x = x`). A function local or parameter keeps the
/// unqualified spelling through the member, its getter, and assignments (`objects/flist.kt`).
/// `this.name` is a member access and is not a use of the local.
pub(super) fn enclosing_value_visible_beside_member(
    file: &File,
    declaration: DeclId,
    name: &str,
    function_local: bool,
) -> bool {
    function_local
        || !anonymous_body_bound_value_names(file, declaration).contains(name)
        || capture_analysis::own_property_initializer_uses_outer_name(file, declaration, name)
}

pub(super) fn anonymous_descendant_writes_name(
    file: &File,
    declaration: DeclId,
    lexical_scope: &AnonymousLexicalClassScope,
    name: &str,
    function_local: bool,
) -> bool {
    anonymous_descendants(declaration, lexical_scope).any(|candidate| {
        enclosing_value_visible_beside_member(file, candidate, name, function_local)
            && (anonymous_body_writes_name(file, candidate, name)
                || candidate != declaration
                    && matches!(file.decl(candidate), Decl::Class(class) if class
                        .interface_delegations
                        .iter()
                        .any(|delegation| expression_writes_name(file, delegation.value, name))))
    })
}

impl Checker<'_> {
    /// What a statement-position local class reads from its enclosing scope.
    ///
    /// Deliberately syntactic and conservative: a name the class also declares is not a capture,
    /// unless an enclosing function local already owns that spelling — the local wins inside the
    /// class, so the class must capture it (`objects/flist.kt`). A name that merely *looks* like a
    /// capture is treated as such. Over-reporting costs an unused constructor parameter (or a
    /// skipped file, when the name is one of the unmodelled kinds); under-reporting emits a class
    /// without the constructor parameter its capture needs.
    pub(super) fn local_class_captures(
        &self,
        scope: &CheckerScope<'_>,
        cl: &ClassDecl,
        anonymous_object: bool,
    ) -> LocalClassCaptures {
        let mut result = LocalClassCaptures::default();
        let mut outer: std::collections::HashSet<String> = std::collections::HashSet::new();
        scope.visit_bindings(Ns::Value, |name, _| {
            outer.insert(name.to_string());
        });
        // A local FUNCTION carries captures of its own; reaching one from a local class would have
        // to compose the two, which is not modelled.
        let mut unsupported: std::collections::HashSet<String> = std::collections::HashSet::new();
        scope.visit_bindings(Ns::Function, |name, _| {
            unsupported.insert(name.to_string());
        });
        // Reaching the enclosing INSTANCE is the second capture kind: the receiver itself, not a
        // binding in the chain. It is carried as ONE capture however many of its members are read,
        // so only the INNERMOST receiver contributes names — that is the object lowering supplies,
        // and reading a member of a further-out receiver would need a CHAIN of captures that is not
        // modelled.
        //
        // It contributes nothing unless that receiver is the DISPATCH receiver, which is what the
        // innermost label being a CLASS label says: lowering supplies the capture from `$dispatch`,
        // so with an extension receiver or a receiver lambda nearer than the enclosing class the
        // checker's `this` and the object handed to the constructor are two different values. Every
        // name then falls through to the value channel, finds no binding, and the class is rejected.
        let innermost_label = self.this_labels.last();
        let enclosing_instance = scope
            .this_ty()
            .filter(|_| innermost_label.is_some_and(|label| label.2));
        let implicit_receiver_capture = scope
            .implicit_receivers_with_declarations()
            .into_iter()
            .next()
            .filter(|(_, _, identity, _)| {
                scope.innermost_class_receiver_identity() != Some(*identity)
            })
            .map(|(ty, extension, identity, class_receiver)| {
                (
                    ty,
                    self.captured_receiver(scope, identity, extension, class_receiver),
                )
            });
        if implicit_receiver_capture.is_some() {
            // Receiver properties are reached through the captured receiver coordinate below; they
            // are not independent lexical values. Keeping both creates an impossible constructor
            // capture for `Receiver.() -> Unit { class Local { val x = receiverProperty } }`.
            outer.retain(|name| {
                !self.lookup(scope, name).is_some_and(|binding| {
                    matches!(
                        binding.origin,
                        ReceiverFnValueOrigin::DispatchProperty { .. }
                    )
                })
            });
        }
        let mut through_outer: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(internal) = enclosing_instance.and_then(Ty::obj_internal) {
            if let Some(class) = self.resolver().classifier(internal) {
                through_outer.extend(class.declared_callables.keys().cloned());
            }
            if let Some(label) = innermost_label {
                through_outer.insert(format!("this@{}", class_declaration_label(&label.0)));
            }
        }
        // A property shadows an enclosing class member of the same spelling, not an enclosing
        // function local. `fun f(head: T) { object { val head get() = head } }` captures `head`.
        for name in cl
            .props
            .iter()
            .map(|p| &p.name)
            .chain(cl.body_props.iter().map(|p| &p.name))
        {
            if self
                .lookup(scope, name)
                .is_some_and(|binding| matches!(binding.origin, ReceiverFnValueOrigin::Local))
            {
                unsupported.remove(name);
                through_outer.remove(name);
                continue;
            }
            outer.remove(name);
            unsupported.remove(name);
            through_outer.remove(name);
        }
        // A member function shadows an enclosing-instance member function, but not a lexical value
        // or local function. Those earlier scope-tower rungs still win call syntax when applicable;
        // in particular an outer receiver-function parameter named `encode` is captured by an
        // anonymous override also named `encode`.
        for name in cl.methods.iter().map(|method| &method.name) {
            through_outer.remove(name);
        }
        // Before a constructor property is stored, and for a plain parameter, the lexical local is
        // nearer than the property. A nested class there captures that value. After the store the
        // nearer binding is the property, so the nested class captures the enclosing instance.
        through_outer.retain(|name| {
            !self
                .lookup(scope, name)
                .is_some_and(|binding| binding.origin == ReceiverFnValueOrigin::Local)
        });
        // Each name belongs to exactly one channel; the nearer binding wins.
        for name in unsupported.iter().chain(&through_outer) {
            outer.remove(name);
        }
        for name in &unsupported {
            through_outer.remove(name);
        }
        // Construction-time reads must travel through the local class constructor just like member
        // reads travel through its fields. This includes an anonymous object written in a super-call:
        // its declaration body is parser-hoisted, so inspect that body's expressions explicitly and
        // make the enclosing local class carry every lexical value the anonymous constructor needs.
        let mut construction_bodies: Vec<ExprId> = Vec::new();
        for p in &cl.body_props {
            construction_bodies.extend(p.init);
        }
        for step in &cl.init_order {
            if let ClassInit::Block(b) = step {
                construction_bodies.push(*b);
            }
        }
        construction_bodies.extend(cl.base_args.iter().copied());
        construction_bodies.extend(
            cl.interface_delegations
                .iter()
                .map(|delegation| delegation.value),
        );
        construction_bodies.extend(cl.props.iter().filter_map(|p| p.default));
        let mut everything = outer.clone();
        everything.extend(unsupported.iter().cloned());
        everything.extend(through_outer.iter().cloned());
        let narrows = scope.local_narrowings();
        let mut captured: Vec<String> = Vec::new();
        let mut needs_outer = false;
        let mut record_construction_uses =
            |body: ExprId,
             visible: &std::collections::HashSet<String>,
             anonymous_super_argument: bool| {
                let body = if anonymous_super_argument {
                    // The forwarded expression runs outside this constructor. A bare captured name
                    // still does not: it is read here, after the value has been passed in.
                    let Some(observed) = anonymous_super_argument_constructor_use(self.file, body)
                    else {
                        return;
                    };
                    observed
                } else {
                    body
                };
                for name in used_names(self.file, body, visible) {
                    if unsupported.contains(&name) {
                        result.unsupported.get_or_insert(name);
                    } else if through_outer.contains(&name) {
                        // An anonymous object's super-constructor argument is evaluated at the
                        // construction site and forwarded. A property read there does not capture
                        // the enclosing instance; the same name in the object body does.
                        if !anonymous_super_argument {
                            needs_outer = true;
                        }
                    } else if !captured.contains(&name) {
                        captured.push(name);
                    }
                }
            };
        for ctor in &cl.secondary_ctors {
            let mut visible = everything.clone();
            for p in &ctor.params {
                visible.remove(&p.name);
            }
            let delegation_args = match &ctor.delegation {
                CtorDelegation::None => &[][..],
                CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
            };
            for body in ctor
                .body
                .into_iter()
                .chain(ctor.params.iter().filter_map(|p| p.default))
                .chain(delegation_args.iter().copied())
            {
                record_construction_uses(body, &visible, false);
            }
        }
        let anonymous_targets = self
            .file
            .anonymous_object_classes
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for &body in &construction_bodies {
            record_construction_uses(
                body,
                &everything,
                anonymous_object && cl.base_args.contains(&body),
            );
            let mut constructions = std::collections::HashSet::new();
            record_expression_targets(self.file, &anonymous_targets, [body], &mut constructions);
            for construction in constructions {
                let Some(&anonymous) = self.file.anonymous_object_classes.get(&construction) else {
                    continue;
                };
                let mut visible = everything.clone();
                for bound in anonymous_body_bound_value_names(self.file, anonymous) {
                    visible.remove(&bound);
                }
                for nested_body in anonymous_body_expressions(self.file, anonymous) {
                    record_construction_uses(nested_body, &visible, false);
                }
            }
        }
        // A classifier nested in a local class is parser-hoisted, but it is still lexically inside
        // that local class. Any enclosing local used by the nested declaration must first be
        // carried by the outer local classifier; an `inner class Inner : Base({ value })` reaches
        // `value` through its captured outer instance. Scan the parser's explicit ownership edge,
        // never internal-name prefixes, so sibling local classes cannot leak captures into one
        // another.
        let local_statement =
            self.file
                .local_class_decls
                .iter()
                .find_map(
                    |(statement, declaration)| match self.file.decl(*declaration) {
                        Decl::Class(candidate) if candidate.span == cl.span => Some(*statement),
                        Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
                    },
                );
        if let Some(nested) =
            local_statement.and_then(|statement| self.file.local_class_nested.get(&statement))
        {
            for declaration in nested {
                let Decl::Class(nested) = self.file.decl(*declaration) else {
                    continue;
                };
                let bodies =
                    nested
                        .base_args
                        .iter()
                        .copied()
                        .chain(
                            nested
                                .interface_delegations
                                .iter()
                                .map(|delegation| delegation.value),
                        )
                        .chain(nested.props.iter().filter_map(|property| property.default))
                        .chain(
                            nested
                                .methods
                                .iter()
                                .filter_map(|method| match method.body {
                                    FunBody::Expr(body) | FunBody::Block(body) => Some(body),
                                    FunBody::None => None,
                                }),
                        )
                        .chain(
                            nested
                                .body_props
                                .iter()
                                .filter_map(|property| property.init),
                        )
                        .chain(nested.body_props.iter().filter_map(
                            |property| match property.getter {
                                Some(FunBody::Expr(body) | FunBody::Block(body)) => Some(body),
                                Some(FunBody::None) | None => None,
                            },
                        ))
                        .chain(nested.init_order.iter().filter_map(|step| match step {
                            ClassInit::Block(body) => Some(*body),
                            ClassInit::PropInit(_) => None,
                        }));
                for body in bodies {
                    record_construction_uses(body, &everything, false);
                }
            }
        }
        // A mutable enclosing local written from any parser-hoisted class body is shared storage,
        // even when the outer function's direct AST walk cannot reach that member expression.
        let mut class_reassigned = std::collections::HashSet::new();
        for body in local_class_capture_expressions(cl) {
            collect_all_reassigned(self.file, body, &mut class_reassigned);
        }
        // Member bodies (including a computed property's accessors, which are methods) may capture.
        let mut member_bodies: Vec<(Vec<String>, ExprId)> = Vec::new();
        for m in &cl.methods {
            if let FunBody::Expr(e) | FunBody::Block(e) = m.body {
                member_bodies.push((m.params.iter().map(|p| p.name.clone()).collect(), e));
            }
        }
        for p in &cl.body_props {
            if let Some(FunBody::Expr(e) | FunBody::Block(e)) = p.getter {
                member_bodies.push((Vec::new(), e));
            }
        }
        for (params, body) in member_bodies {
            let mut visible = everything.clone();
            for p in &params {
                visible.remove(p);
            }
            for name in used_names(self.file, body, &visible) {
                if unsupported.contains(&name) {
                    result.unsupported.get_or_insert(name);
                } else if through_outer.contains(&name) {
                    needs_outer = true;
                } else if !captured.contains(&name) {
                    captured.push(name);
                }
            }
        }
        // The enclosing instance goes FIRST: lowering identifies it by POSITION (field 0), which is
        // what both an outer member read and a `this@Outer` go through.
        if needs_outer {
            match enclosing_instance {
                Some(outer) => result.values.push(AnonymousObjectCapture {
                    name: "this$0".to_string(),
                    ty: outer,
                    shared_cell: false,
                    storage_ty: None,
                    source: AnonymousObjectCaptureSource::EnclosingInstance {
                        current: true,
                        depth: 0,
                    },
                    receiver_label: None,
                    receiver: Some(crate::fir::FirCapturedReceiver::Enclosing),
                    lexical_shadow_depth: 0,
                    capture_dependency: None,
                }),
                None => {
                    result.unsupported.get_or_insert("this".to_string());
                }
            }
        }
        // A local classifier is emitted as a separate body unit, so an enclosing receiver-lambda
        // or extension receiver must cross the same constructor/field boundary as a lexical value.
        // Keep the exact receiver-tower coordinate selected at the declaration site. Capturing it
        // conservatively is harmless when no member ultimately reads it and prevents a later body
        // callback from attempting source-scope lookup after the enclosing body has been dropped.
        if let Some((receiver, receiver_name)) = implicit_receiver_capture {
            result.values.push(AnonymousObjectCapture {
                name: "this$receiver".to_string(),
                ty: receiver,
                shared_cell: false,
                storage_ty: None,
                source: AnonymousObjectCaptureSource::ImplicitReceiver {
                    current: true,
                    depth: 0,
                },
                receiver_label: innermost_label
                    .filter(|(_, _, is_class)| !*is_class)
                    .map(|(label, _, _)| label.clone().into_boxed_str()),
                receiver: Some(receiver_name),
                lexical_shadow_depth: 0,
                capture_dependency: None,
            });
        }
        captured.sort();
        if anonymous_object {
            let lexical = captured
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            for argument in &cl.base_args {
                if self.anonymous_super_argument_stays(*argument, &lexical) {
                    continue;
                }
                result.forwarded_super_arguments.push(*argument);
            }
        }
        for name in captured {
            let Some(local) = self.lookup(scope, &name) else {
                result.unsupported.get_or_insert(name);
                continue;
            };
            let source = match local.origin {
                ReceiverFnValueOrigin::ClassStorage(field)
                | ReceiverFnValueOrigin::EnumEntryPropertyStorage { field, .. } => {
                    AnonymousObjectCaptureSource::ClassStorage { field }
                }
                ReceiverFnValueOrigin::Local
                | ReceiverFnValueOrigin::DispatchProperty { .. }
                | ReceiverFnValueOrigin::TopLevelProperty => {
                    AnonymousObjectCaptureSource::LexicalValue
                }
            };
            result.values.push(AnonymousObjectCapture {
                // Smart-cast state is a fact about this control-flow point, not the type of a
                // mutable cell captured by a separately checked classifier body.
                ty: if local.is_var {
                    local.ty
                } else {
                    narrows.get(&name).copied().unwrap_or(local.ty)
                },
                shared_cell: CapturedBinding {
                    delegated: local.delegate_storage_ty.is_some(),
                    mutable: local.is_var,
                    already_shared: local.shared_storage_cell,
                    written_here: self.fn_reassigned.contains(&name)
                        || class_reassigned.contains(&name),
                }
                .is_shared_cell(),
                storage_ty: local.delegate_storage_ty,
                name,
                source,
                receiver_label: None,
                receiver: None,
                lexical_shadow_depth: 0,
                capture_dependency: None,
            });
        }
        result
    }

    /// A super-constructor argument stays in the anonymous constructor when this walk already
    /// recorded its bare name as a lexical capture, or when checking has accepted it as a
    /// compile-time constant. A missing type or `Ty::Error` is not a constant, so the argument
    /// is evaluated at the construction site instead.
    fn anonymous_super_argument_stays(
        &self,
        argument: ExprId,
        lexical: &std::collections::HashSet<&str>,
    ) -> bool {
        if let Some(observed) = anonymous_super_argument_constructor_use(self.file, argument) {
            if let Expr::Name(name) = self.file.expr(observed) {
                if lexical.contains(name.as_str()) {
                    return true;
                }
            }
        }
        let mut core = argument;
        loop {
            match self.file.expr(core) {
                Expr::As { operand, .. } | Expr::NotNull { operand } => core = *operand,
                _ => break,
            }
        }
        let Some(ty) = self.expr_types.get(core.0 as usize).copied() else {
            return false;
        };
        if ty == Ty::Error || ty.mentions_error() {
            return false;
        }
        checked_constant_expression(
            CheckedConstantExpression {
                file: self.file,
                expression_types: &self.expr_types,
                resolved_constants: &self.resolved_constants,
                resolved_calls: &self.resolved_calls,
                resolved_operator_calls: &self.resolved_operator_calls,
            },
            core,
            ty,
        )
        .is_some()
    }

    /// Publish super-constructor arguments of an expression-position anonymous object after its
    /// arguments have been typed. Statement-position objects publish from the local-class capture
    /// walk; both use the same stay rule while the lexical bindings are still in scope.
    pub(super) fn publish_anonymous_super_forwards(
        &mut self,
        scope: &CheckerScope<'_>,
        declaration: DeclId,
        arguments: &[ExprId],
    ) {
        let mut lexical = std::collections::HashSet::new();
        scope.visit_bindings(Ns::Value, |name, binding| {
            let Some(local) = binding.value() else {
                return;
            };
            if matches!(
                local.origin,
                ReceiverFnValueOrigin::Local
                    | ReceiverFnValueOrigin::ClassStorage(_)
                    | ReceiverFnValueOrigin::EnumEntryPropertyStorage { .. }
            ) {
                lexical.insert(name.to_string());
            }
        });
        let lexical = lexical
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut forwarded = Vec::new();
        for argument in arguments {
            if self.anonymous_super_argument_stays(*argument, &lexical) {
                continue;
            }
            forwarded.push(*argument);
        }
        if forwarded.is_empty() {
            self.discovered_anonymous_super_forwards
                .remove(&declaration);
        } else {
            self.discovered_anonymous_super_forwards
                .insert(declaration, forwarded);
        }
    }
}
