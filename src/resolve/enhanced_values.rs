//! Where the values a checked body produces carry kotlinc's `EnhancedNullability` attribute.
//!
//! A selected declaration states which positions of its declared result are marked
//! ([`TypeEnhancement`], set where a Java result is enhanced from the declarations it overrides).
//! A call's value is that declared result with its type parameters substituted, and FIR keeps an
//! argument's marks through the substitution: a class type parameter takes the marks of the
//! receiver's type argument, seen as the declaring class, and a callable's own type parameter the
//! marks its constraints agree on. `hashMap.entries.iterator().next()` is therefore a marked
//! entry: `entries` marks its element type, `iterator()` returns `MutableIterator<E>` over it, and
//! `next()` returns that `E`. A local whose type is inferred from such a value keeps the marks of
//! its type arguments, not of its head.
//!
//! The marks are read from selected call identities and their committed source-argument mappings
//! after typing, so they never take part in selection or inference;
//! [`super::platform_value_narrowing`] consumes them to decide kotlinc's implicit not-null casts.

use super::{Checker, Local, ReceiverFnValueOrigin, ResolvedCall, ResolvedConstructor};
use crate::ast::{Expr, ExprId};
use crate::libraries::{GenericSig, ResultEnhancement, TypeEnhancement};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// The checker's record of marked values that a later check reads back through a local.
#[derive(Default)]
pub(super) struct EnhancedLocals {
    /// The marks of each local declared with a type inferred from a marked value, by its flow
    /// identity.
    declarations: HashMap<u32, TypeEnhancement>,
    /// The marks of each read of such a local.
    reads: HashMap<ExprId, TypeEnhancement>,
}

impl Checker<'_> {
    /// Record the marks the local, just declared without a type, takes from its
    /// `initializer`.
    pub(super) fn record_enhanced_local(&mut self, flow_identity: u32, initializer: ExprId) {
        let marks = self.value_enhancement(initializer).declared();
        if marks.is_none() {
            return;
        }
        self.enhanced_locals
            .declarations
            .insert(flow_identity, marks);
    }

    /// Record the marks the loop variable takes from the elements of `iterable`.
    pub(super) fn record_enhanced_loop_variable(&mut self, flow_identity: u32, iterable: ExprId) {
        let Some(protocol) = self.iterator_protocols.get(&iterable) else {
            return;
        };
        let marks = protocol.enhancement.element.declared();
        if marks.is_none() {
            return;
        }
        self.enhanced_locals
            .declarations
            .insert(flow_identity, marks);
    }

    /// Record that `read` reads `local` at its declared type.
    pub(super) fn record_enhanced_local_read(&mut self, read: ExprId, local: &Local) {
        if !matches!(local.origin, ReceiverFnValueOrigin::Local) || local.ty != local.declared_ty {
            return;
        }
        if let Some(marks) = self.enhanced_locals.declarations.get(&local.flow_identity) {
            self.enhanced_locals.reads.insert(read, marks.clone());
        }
    }

    /// The calls whose value is itself marked, for checks that read a branch's value after
    /// checking.
    pub(super) fn enhanced_value_heads(&self) -> std::collections::HashSet<ExprId> {
        self.resolved_calls
            .keys()
            .copied()
            .filter(|&call| self.value_enhancement(call).head())
            .collect()
    }

    /// The marks of the value `e` produces.
    pub(super) fn value_enhancement(&self, e: ExprId) -> TypeEnhancement {
        if let Some(marks) = self.enhanced_locals.reads.get(&e) {
            return marks.clone();
        }
        if let Some(constructor) = self.resolved_constructors.get(&e) {
            return self.constructor_result_enhancement(e, constructor);
        }
        let Some(call) = self.resolved_calls.get(&e) else {
            return TypeEnhancement::NONE;
        };
        let receiver = self.call_receiver(e).map(|receiver| {
            (
                self.expr_types[receiver.0 as usize],
                self.value_enhancement(receiver),
            )
        });
        let explicit_type_arguments = self.file.call_type_args.contains_key(&e.0);
        let arguments = (!explicit_type_arguments)
            .then(|| self.resolved_call_arg_slots.get(&e))
            .flatten();
        self.call_result_enhancement(call, receiver, arguments)
    }

    /// The marks of the object the constructor call `e` creates: its classifier's type arguments
    /// inferred from the arguments take their marks (`Box(entries.firstOf())` is a box of a marked
    /// entry). The created object itself is never marked.
    fn constructor_result_enhancement(
        &self,
        e: ExprId,
        constructor: &ResolvedConstructor,
    ) -> TypeEnhancement {
        let member = match constructor {
            ResolvedConstructor::Plain { member, .. }
            | ResolvedConstructor::PlainSlots { member, .. } => member,
            ResolvedConstructor::Synthetic { ctor, .. } => &ctor.declaration,
            ResolvedConstructor::Source {
                owner,
                declaration_params,
                argument_bindings,
                ..
            } => {
                if self.file.call_type_args.contains_key(&e.0) {
                    return TypeEnhancement::NONE;
                }
                let Some(classifier) = self.resolver().classifier(*owner) else {
                    return TypeEnhancement::NONE;
                };
                let formals = classifier.type_params();
                let mut bindings = HashMap::new();
                bind_type_parameters(
                    self,
                    formals,
                    declaration_params,
                    None,
                    argument_bindings,
                    &mut bindings,
                );
                let argument_count = self.expr_types[e.0 as usize].type_args().len();
                return TypeEnhancement::new(
                    false,
                    formals
                        .iter()
                        .take(argument_count)
                        .map(|name| bindings.get(name).cloned().unwrap_or_default())
                        .collect(),
                );
            }
        };
        let Some(signature) = member.generic_sig.as_ref() else {
            return TypeEnhancement::NONE;
        };
        if self.file.call_type_args.contains_key(&e.0) {
            return TypeEnhancement::NONE;
        }
        let Some(arguments) = self.resolved_call_arg_slots.get(&e) else {
            return TypeEnhancement::NONE;
        };
        let mut bindings = HashMap::new();
        bind_callable_parameters(self, signature, None, arguments, &mut bindings);
        TypeEnhancement::NONE.substitute(signature.ret, &|name| bindings.get(name).cloned())
    }

    /// The marks of the value `call` produces on a `receiver` of the given type and marks, its
    /// own type parameters inferred from `arguments` when they were inferred from them alone.
    pub(super) fn call_result_enhancement(
        &self,
        call: &ResolvedCall,
        receiver: Option<(Ty, TypeEnhancement)>,
        arguments: Option<&super::SelectedArgumentCommitment>,
    ) -> TypeEnhancement {
        let not_null = |enhancement| enhancement == ResultEnhancement::NotNull;
        let (signature, result, enhanced, receiver_parameter) = match call {
            ResolvedCall::Member(selected) => (
                selected.member.generic_sig.as_ref(),
                selected.member.ret,
                (
                    &selected.member.enhanced_result,
                    not_null(selected.member.call_sig.result_enhancement),
                ),
                false,
            ),
            ResolvedCall::Companion(member) => (
                member.generic_sig.as_ref(),
                member.ret,
                (
                    &member.enhanced_result,
                    not_null(member.call_sig.result_enhancement),
                ),
                false,
            ),
            ResolvedCall::TopLevel(selected) => (
                selected.callable.generic_sig.as_deref(),
                selected.callable.ret,
                (
                    &selected.callable.enhanced_result,
                    not_null(selected.call_sig.result_enhancement),
                ),
                false,
            ),
            ResolvedCall::Extension(selected) => (
                selected.callable.generic_sig.as_deref(),
                selected.callable.ret,
                (&selected.callable.enhanced_result, false),
                true,
            ),
            _ => return TypeEnhancement::NONE,
        };
        let (enhanced, head) = enhanced;
        let marks = enhanced.marks.with_head(head);
        let mut bindings = HashMap::new();
        // A specialized member's result no longer names its classifier's type parameters; the
        // result its declaring classifier states does.
        let declared = match (&enhanced.declared, &receiver) {
            (Some((classifier, declared)), Some((receiver_ty, receiver_marks))) => {
                self.bind_class_parameters(
                    *classifier,
                    *receiver_ty,
                    receiver_marks,
                    &mut bindings,
                );
                *declared
            }
            _ => signature.map_or(result, |signature| signature.ret),
        };
        if let (Some(signature), Some(arguments)) = (signature, arguments) {
            let receiver = receiver.filter(|_| receiver_parameter);
            bind_callable_parameters(self, signature, receiver.as_ref(), arguments, &mut bindings);
        }
        if bindings.is_empty() {
            return marks;
        }
        marks.substitute(declared, &|name| bindings.get(name).cloned())
    }

    /// The explicit source receiver of the selected call or property read `e`.
    fn call_receiver(&self, e: ExprId) -> Option<ExprId> {
        match self.file.expr(e) {
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => Some(*receiver),
                _ => None,
            },
            Expr::SafeCall { receiver, .. } | Expr::Member { receiver, .. } => Some(*receiver),
            _ => None,
        }
    }

    /// Bind the type parameters of `owner` to the marks of the matching type arguments of a
    /// receiver of type `receiver` with `marks`, seen as `owner`.
    fn bind_class_parameters(
        &self,
        owner: TypeName,
        receiver: Ty,
        marks: &TypeEnhancement,
        bindings: &mut HashMap<String, TypeEnhancement>,
    ) {
        let Some((_, view)) = self.supertype_view(receiver, marks, owner) else {
            return;
        };
        let Some(class) = self.resolver().classifier(owner) else {
            return;
        };
        for (index, parameter) in class.type_params().iter().enumerate() {
            let argument = view.argument(index);
            if !argument.is_none() {
                bindings.insert(parameter.clone(), argument.clone());
            }
        }
    }

    /// A value of type `actual` with `marks`, seen as its supertype `target`: that supertype as
    /// `actual` applies it, and its marks. A supertype's type arguments are expressions over the
    /// subtype's own parameters, so the marks follow by substituting those parameters' marks.
    fn supertype_view(
        &self,
        actual: Ty,
        marks: &TypeEnhancement,
        target: TypeName,
    ) -> Option<(Ty, TypeEnhancement)> {
        let actual = actual.non_null();
        let class = actual.kotlin_class_internal()?;
        if class == target {
            return Some((actual, marks.clone()));
        }
        let source = self.fed_source();
        let (_, applied, _) = crate::symbol_resolver::applied_hierarchy(&source, actual)
            .into_iter()
            .find(|(owner, _, _)| *owner == target)?;
        if !marks.has_marked_arguments() {
            return Some((applied, TypeEnhancement::new(marks.head(), Vec::new())));
        }
        let declaration = self.resolver().classifier(class)?;
        let parameters = declaration.type_params();
        let any = Ty::nullable(Ty::obj("kotlin/Any"));
        let symbolic = Ty::obj_args_name(
            class,
            &parameters
                .iter()
                .map(|parameter| Ty::ty_param(parameter, any))
                .collect::<Vec<_>>(),
        );
        let (_, symbolic_view, _) = crate::symbol_resolver::applied_hierarchy(&source, symbolic)
            .into_iter()
            .find(|(owner, _, _)| *owner == target)?;
        let parameter_marks = |name: &str| {
            parameters
                .iter()
                .position(|parameter| parameter == name)
                .map(|index| marks.argument(index).clone())
        };
        let view_marks = TypeEnhancement::new(
            marks.head(),
            symbolic_view
                .type_args()
                .iter()
                .map(|&argument| TypeEnhancement::NONE.substitute(argument, &parameter_marks))
                .collect(),
        );
        Some((applied, view_marks))
    }
}

/// Bind a callable's own type parameters to the marks of the values its declared receiver and
/// value parameters take. A parameter constrained by several values is marked only where all of
/// them are; one constrained by none is left unbound.
fn bind_callable_parameters(
    checker: &Checker<'_>,
    signature: &GenericSig,
    receiver: Option<&(Ty, TypeEnhancement)>,
    arguments: &super::SelectedArgumentCommitment,
    bindings: &mut HashMap<String, TypeEnhancement>,
) {
    bind_type_parameters(
        checker,
        &signature.formals,
        &signature.params,
        receiver
            .zip(signature.receiver.as_ref())
            .map(|(actual, declared)| (*declared, actual)),
        &arguments.argument_bindings,
        bindings,
    );
}

fn bind_type_parameters(
    checker: &Checker<'_>,
    formals: &[String],
    parameters: &[Ty],
    receiver: Option<(Ty, &(Ty, TypeEnhancement))>,
    arguments: &[super::SelectedArgumentBinding],
    bindings: &mut HashMap<String, TypeEnhancement>,
) {
    if formals.is_empty() {
        return;
    }
    let mut constraints: HashMap<String, Vec<TypeEnhancement>> = HashMap::new();
    if let Some((declared, (actual, marks))) = receiver {
        constrain(checker, formals, declared, *actual, marks, &mut constraints);
    }
    for binding in arguments {
        let Some(&parameter) = parameters.get(binding.parameter) else {
            continue;
        };
        let declared = if binding.vararg_element {
            parameter.array_read_elem().unwrap_or(parameter)
        } else {
            parameter
        };
        let actual = checker.expr_types[binding.argument.0 as usize];
        let marks = checker.value_enhancement(binding.argument);
        constrain(checker, formals, declared, actual, &marks, &mut constraints);
    }
    for (name, marks) in constraints {
        let met = marks
            .iter()
            .skip(1)
            .fold(marks[0].clone(), |met, next| met.meet(next));
        if !met.is_none() {
            bindings.insert(name, met);
        }
    }
}

/// Collect the marks a value of type `actual` with `marks` gives the callable's type parameters
/// occurring in the `declared` parameter type it is passed to.
fn constrain(
    checker: &Checker<'_>,
    formals: &[String],
    declared: Ty,
    actual: Ty,
    marks: &TypeEnhancement,
    constraints: &mut HashMap<String, Vec<TypeEnhancement>>,
) {
    match declared {
        Ty::TyParam(name, _) if formals.iter().any(|formal| formal == name) => {
            constraints
                .entry(name.to_string())
                .or_default()
                .push(marks.clone());
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::DefinitelyNotNull(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => {
            constrain(checker, formals, *inner, actual, marks, constraints);
        }
        Ty::Obj(class, declared_arguments) if !declared_arguments.is_empty() => {
            let actual = match actual.non_null() {
                Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                other => other,
            };
            let Some((view, view_marks)) = checker.supertype_view(actual, marks, class) else {
                return;
            };
            for (index, (&declared, &actual)) in
                declared_arguments.iter().zip(view.type_args()).enumerate()
            {
                constrain(
                    checker,
                    formals,
                    declared,
                    actual,
                    view_marks.argument(index),
                    constraints,
                );
            }
        }
        _ => {}
    }
}
