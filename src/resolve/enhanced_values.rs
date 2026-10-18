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
use crate::ast::ExprId;
use crate::fx_hash::HashMap;
use crate::libraries::{GenericSig, TypeEnhancement};
use crate::types::{Ty, TypeName};

/// The checker's record of marked values that a later check reads back through a local.
#[derive(Default)]
pub(super) struct EnhancedLocals {
    /// The marks of each local declared with a type inferred from a marked value, by its flow
    /// identity.
    declarations: HashMap<u32, EnhancedLocalDeclaration>,
    /// The marks of each read of such a local.
    reads: HashMap<ExprId, TypeEnhancement>,
}

#[derive(Clone)]
struct EnhancedLocalDeclaration {
    /// The semantic storage type this publication fact describes. Mutable-local assignments may
    /// change the value's marks, but they cannot change this inferred declaration type.
    declared: Ty,
    value: TypeEnhancement,
}

impl Checker<'_> {
    /// Record the marks the local, just declared without a type, takes from its
    /// `initializer`.
    pub(super) fn record_enhanced_local(
        &mut self,
        flow_identity: u32,
        initializer: ExprId,
        declared: Ty,
    ) {
        let marks = self.value_enhancement(initializer).declared();
        if marks.is_none() {
            self.enhanced_locals.declarations.remove(&flow_identity);
            return;
        }
        self.enhanced_locals.declarations.insert(
            flow_identity,
            EnhancedLocalDeclaration {
                declared,
                value: marks,
            },
        );
    }

    /// Record the marks the loop variable takes from the elements of `iterable`.
    pub(super) fn record_enhanced_loop_variable(
        &mut self,
        flow_identity: u32,
        iterable: ExprId,
        declared: Ty,
    ) {
        let Some(protocol) = self.iterator_protocols.get(&iterable) else {
            self.enhanced_locals.declarations.remove(&flow_identity);
            return;
        };
        let marks = protocol.enhancement.element.declared();
        if marks.is_none() {
            self.enhanced_locals.declarations.remove(&flow_identity);
            return;
        }
        self.enhanced_locals.declarations.insert(
            flow_identity,
            EnhancedLocalDeclaration {
                declared,
                value: marks,
            },
        );
    }

    /// Meet a mutable local's declaration marks with those of its replacement value. The ordinary
    /// data-flow engine owns whether the write executes and the current flow type; this sidecar
    /// retains only the enhancement positions shared by every live value stored in the inferred
    /// declaration. A differently typed subtype cannot align its argument positions with the
    /// declaration without a checked supertype projection, and a compound write has no checked
    /// replacement value of the local's type; both conservatively invalidate the sidecar fact.
    pub(super) fn update_enhanced_local(&mut self, local: &Local, value: Option<ExprId>) {
        let assigned = value
            .filter(|value| self.expr_types[value.0 as usize] == local.declared_ty)
            .map(|value| self.value_enhancement(value).declared())
            .unwrap_or_default();
        let retained = matches!(local.origin, ReceiverFnValueOrigin::Local)
            .then(|| self.enhanced_locals.declarations.get(&local.flow_identity))
            .flatten()
            .filter(|fact| {
                local.is_var
                    && local.write_ty == Some(local.declared_ty)
                    && fact.declared == local.declared_ty
            })
            .map(|fact| fact.value.meet(&assigned));
        if let Some(marks) = retained.filter(|marks| !marks.is_none()) {
            self.enhanced_locals
                .declarations
                .get_mut(&local.flow_identity)
                .expect("retained enhancement declaration")
                .value = marks;
        } else {
            self.enhanced_locals
                .declarations
                .remove(&local.flow_identity);
        }
    }

    /// Record that `read` reads `local` at its declared type.
    pub(super) fn record_enhanced_local_read(&mut self, read: ExprId, local: &Local) {
        if !matches!(local.origin, ReceiverFnValueOrigin::Local) || local.ty != local.declared_ty {
            self.enhanced_locals.reads.remove(&read);
            return;
        }
        if let Some(fact) = self
            .enhanced_locals
            .declarations
            .get(&local.flow_identity)
            .filter(|fact| fact.declared == local.declared_ty)
        {
            self.enhanced_locals.reads.insert(read, fact.value.clone());
        } else {
            self.enhanced_locals.reads.remove(&read);
        }
    }

    /// The calls whose value is itself marked, for checks that read a branch's value after
    /// checking.
    pub(super) fn enhanced_value_heads(&self) -> crate::fx_hash::HashSet<ExprId> {
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
        let receiver = self.checked_call_receiver(e).map(|receiver| {
            (
                self.expr_types[receiver.0 as usize],
                self.value_enhancement(receiver),
            )
        });
        let explicit_type_arguments = self.has_checked_explicit_type_arguments(e);
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
                ..
            } => {
                if self.has_checked_explicit_type_arguments(e) {
                    return TypeEnhancement::NONE;
                }
                let Some(arguments) = self.resolved_call_arg_slots.get(&e) else {
                    return TypeEnhancement::NONE;
                };
                let Some(classifier) = self.resolver().classifier(*owner) else {
                    return TypeEnhancement::NONE;
                };
                let formals = classifier.type_params();
                let mut bindings = HashMap::default();
                bind_type_parameters(
                    self,
                    formals,
                    declaration_params,
                    None,
                    &arguments.argument_bindings,
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
        if self.has_checked_explicit_type_arguments(e) {
            return TypeEnhancement::NONE;
        }
        let Some(arguments) = self.resolved_call_arg_slots.get(&e) else {
            return TypeEnhancement::NONE;
        };
        let mut bindings = HashMap::default();
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
        let target = call.selected_result_target();
        let Some((enhanced, head)) = target.enhancement else {
            return TypeEnhancement::NONE;
        };
        let marks = enhanced.with_head(head);
        let mut bindings = HashMap::default();
        if let (
            Some(super::selected_result_publication::DispatchBinding::Explicit(owner)),
            Some((receiver_ty, receiver_marks)),
        ) = (target.dispatch, &receiver)
        {
            self.bind_class_parameters(owner, *receiver_ty, receiver_marks, &mut bindings);
        }
        if let (Some(signature), Some(arguments)) = (target.signature, arguments) {
            let receiver = receiver.filter(|_| target.receiver_parameter);
            bind_callable_parameters(self, signature, receiver.as_ref(), arguments, &mut bindings);
        }
        if bindings.is_empty() {
            marks
        } else {
            marks.substitute(target.declared, &|name| bindings.get(name).cloned())
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
    let mut constraints: HashMap<String, Vec<TypeEnhancement>> = HashMap::default();
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
    super::selected_result_publication::constrain_parameters(
        formals,
        declared,
        actual,
        marks,
        constraints,
        super::selected_result_publication::StarProjectionPolicy::Stop,
        &mut |actual, marks, owner| {
            let actual = match actual.non_null() {
                Ty::InProjection(inner) | Ty::OutProjection(inner) => *inner,
                other => other,
            };
            let (view, view_marks) = checker.supertype_view(actual, marks, owner)?;
            let facts = (0..view.type_args().len())
                .map(|index| view_marks.argument(index).clone())
                .collect();
            Some((view, facts))
        },
    );
}
