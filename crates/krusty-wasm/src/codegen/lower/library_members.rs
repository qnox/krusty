//! Calls of a library classifier's member that dispatch through the program
//! (`crate::interface_dispatch`): an interface's member no dependency publishes a body for
//! (`CharSequence.length` on a `CharSequence`), a class's member that builtin classes override
//! (`Number.toLong()` on a `Number`), and an abstract member of a library class
//! (`IntIterator.nextInt()` on an `IntIterator`), which the program's subclasses implement.
//!
//! The member is named by its KLIB identity, which a class of the program implementing it records
//! its override under (`super::super::interface_members`); the builtin classes' implementations
//! come from what checked FIR froze on the selected declaration (`crate::builtin_members`).
//! A member of `kotlin.Any` is realized through the receiver's own representation instead
//! (`super::any_members`), and a member whose body a dependency publishes is called through the
//! entry its module exports. A published class dispatches through its producer-owned class table
//! even when a concrete member also has an executable export, preserving virtual overrides.

use krusty::backend::interface_identity::property_member_identity;
use krusty::backend::{BackendCallableFact, BackendPropertyFact, CheckedBackendCallables};
use krusty::libraries::ExternalCallableKind;
use krusty::libraries::MemberRealization;
use krusty::types::{SemanticCallableOwner, TypeName};

use super::super::super::objects::{slot_field, REFERENCE};
use super::super::interface_members::{accessor_types, member_signature, Signature};
use super::*;

pub(in crate::codegen) mod published_class;

/// Positive checked operations whose runtime realization owns the call in both phases.
pub(in crate::codegen) fn target_owns_call(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
    fact: &BackendCallableFact,
) -> bool {
    super::declared_by_string_builder(fact)
        || super::declared_by_array_list(fact)
        || array_list_realizes(ir, callables, expression, fact)
        || super::any_members::call_member(ir, callables, expression).is_some()
}

/// Whether the runtime realizes `fact` on the `ArrayList` receiver of `expression`, including
/// members the list inherits from the abstract collection classes.
fn array_list_realizes(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
    fact: &BackendCallableFact,
) -> bool {
    let receiver = match ir.expr(expression) {
        IrExpr::Call {
            dispatch_receiver, ..
        } => *dispatch_receiver,
        IrExpr::Checked(operation) => callables
            .dependency_accessor(operation)
            .and_then(|access| access.receiver),
        _ => None,
    };
    receiver.is_some_and(|receiver| {
        super::array_list_receiver(ir, receiver) && super::realized_array_list_member(fact)
    })
}

/// How a library member is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::codegen) enum MemberRole {
    Function,
    Getter,
    Setter,
}

/// The identity naming the member `fact` declares, in `role`: a function's own, or the getter's
/// or setter's of the property `fact` is an accessor of.
pub(in crate::codegen) fn library_member_identity(
    fact: &BackendCallableFact,
    role: MemberRole,
) -> Option<Vec<u8>> {
    let identity = fact.dependency_callable.as_ref()?;
    match role {
        MemberRole::Function => fact.member_slot_identity(),
        MemberRole::Getter => property_member_identity(identity, false),
        MemberRole::Setter => property_member_identity(identity, true),
    }
}

/// The library classifier declaring the member `fact`.
pub(in crate::codegen) fn library_member_owner(fact: &BackendCallableFact) -> Option<TypeName> {
    match fact.declaration_owner {
        Some(SemanticCallableOwner::Classifier(owner)) => Some(owner),
        _ => None,
    }
}

/// The signature the member `fact` declares is carried as in `role`, from its declared parameter
/// and result types (a type parameter's value is a reference); a setter's from its property's
/// getter, which `fact` is then.
pub(in crate::codegen) fn library_member_signature(
    fact: &BackendCallableFact,
    role: MemberRole,
) -> Result<Signature, Unsupported> {
    let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
    let result = fact.declared_ret.unwrap_or(fact.ret);
    match role {
        MemberRole::Function | MemberRole::Getter => member_signature(params, result),
        MemberRole::Setter => {
            let (params, result) = accessor_types(result, true);
            member_signature(&params, result)
        }
    }
}

/// Whether a call of `fact` with a dispatch receiver at `expression` goes through its library
/// member: a member of a library classifier that dispatches on its receiver, with no `published`
/// body to call and no compiler operation realizing it (`KClass.simpleName`), which an
/// interface's classes or the builtin classes below it implement.
pub(in crate::codegen) fn dispatches_through_library_member(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
    fact: &BackendCallableFact,
    published: bool,
) -> bool {
    !published
        && fact.kind == ExternalCallableKind::Member
        && fact.member_realization == MemberRealization::Dispatch
        && fact.compiler_operation().is_none()
        && fact.dependency_callable.is_some()
        && library_member_owner(fact).is_some()
        && (fact.owner_is_interface
            || !fact.builtin_implementations.is_empty()
            || is_abstract_class_member(fact))
        && any_members::call_member(ir, callables, expression).is_none()
}

/// Whether `property` is `KCallable.name`, which every callable-reference object implements
/// (`crate::callable_references`).
fn is_reference_name(property: &BackendPropertyFact) -> bool {
    &*property.name == "name" && property.owner.matches("kotlin/reflect/KCallable")
}

/// Whether `fact` is an abstract member of a library class: no body of its own runs, only the
/// overrides the receiver's class and its superclasses supply, as an interface's member.
pub(in crate::codegen) fn is_abstract_class_member(fact: &BackendCallableFact) -> bool {
    !fact.owner_is_interface
        && fact.kind == ExternalCallableKind::Member
        && fact.member_realization == MemberRealization::Dispatch
        && library_member_owner(fact).is_some()
        && fact
            .klib_body_declaration_modifiers
            .is_some_and(|modifiers| modifiers.is_abstract())
}

impl Body<'_> {
    /// Emit `id` when it is a call through a library member.
    pub(super) fn library_member_expression(
        &mut self,
        id: u32,
        node: &IrExpr,
    ) -> Option<Result<(), Unsupported>> {
        let (target, receiver, args, (property, role)) = match node {
            IrExpr::Call {
                callee,
                dispatch_receiver: Some(receiver),
                args,
            } => {
                let Callee::External {
                    target,
                    defaults,
                    extension_receiver_parameter: None,
                    ..
                } = &**callee
                else {
                    return None;
                };
                if !defaults.is_empty() {
                    return None;
                }
                (
                    *target,
                    *receiver,
                    args.as_slice(),
                    (None, MemberRole::Function),
                )
            }
            IrExpr::Checked(operation) => {
                let access = self.callables.dependency_accessor(operation)?;
                let receiver = access.receiver?;
                if !access.dispatches() || access.callable.is_member_extension() {
                    return None;
                }
                (
                    access.target,
                    receiver,
                    access.arguments,
                    (
                        Some(access.property),
                        if access.writes() {
                            MemberRole::Setter
                        } else {
                            MemberRole::Getter
                        },
                    ),
                )
            }
            _ => return None,
        };
        let fact = self.callables.callable(target)?;
        if target_owns_call(self.ir, self.callables, id, fact) {
            return None;
        }
        let callables = self.callables;
        let application = || match node {
            IrExpr::Call { callee, .. } => Ok((**callee).clone()),
            IrExpr::Checked(operation) => callables
                .dependency_accessor(operation)
                .ok_or_else(|| {
                    "a checked class accessor without its frozen declaration".to_owned()
                })?
                .callee()
                .map_err(str::to_owned),
            _ => Err("a published class member without its checked application".into()),
        };
        if let Some(member) = published_class::select(
            fact,
            role,
            application,
            self.classifiers,
            self.laid_out_classes,
            self.module,
        ) {
            let fact = fact.clone();
            return Some(
                member.and_then(|member| {
                    self.published_class_call(id, &fact, member, receiver, args)
                }),
            );
        }
        let published = self.calls.external(target).is_some();
        if !dispatches_through_library_member(self.ir, self.callables, id, fact, published) {
            return None;
        }
        // A function's identity names a function, and an accessor's a property.
        let identity = library_member_identity(fact, role)?;
        Some(self.library_member_call(id, fact.clone(), identity, (property, role), receiver, args))
    }

    fn published_class_call(
        &mut self,
        id: u32,
        fact: &BackendCallableFact,
        member: published_class::PublishedMember,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
        let result = fact.declared_ret.unwrap_or(fact.ret);
        if params.len() != args.len() {
            return Err("a class member call whose arguments do not match its parameters".into());
        }
        let this = self.local(REFERENCE);
        self.value(receiver, objects::any())?;
        self.code.local_tee(this);
        for ((&argument, &applied), &parameter) in args
            .iter()
            .zip(member.applied_parameters.iter())
            .zip(params)
        {
            self.value(argument, applied)?;
            self.coerce_emitted_value(applied, parameter)?;
        }
        self.code.local_get(this);
        self.table_of_object();
        self.code
            .ref_cast(false, HeapType::Concrete(member.table))
            .struct_get(member.table, slot_field(member.slot))
            .call_ref(member.signature);
        self.coerce_emitted_value(result, member.applied_result)?;
        match self.type_of(id)? {
            Some(ty) => self.coerce_emitted_value(member.applied_result, ty),
            None => {
                self.code.unreachable();
                Ok(())
            }
        }
    }

    /// A call through the library member `fact` declares, in `role`: the getter or setter of
    /// `property` when there is one.
    fn library_member_call(
        &mut self,
        id: u32,
        fact: BackendCallableFact,
        identity: Vec<u8>,
        (property, role): (Option<&BackendPropertyFact>, MemberRole),
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
        let result = fact.declared_ret.unwrap_or(fact.ret);
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        // A setter's member is named and carried as its property's getter declares it, the way
        // an implementing class records it (`super::super::interface_members`).
        let signature = match (property, role) {
            (Some(property), MemberRole::Setter) => {
                let getter = self
                    .callables
                    .callable(property.getter)
                    .ok_or("a dependency setter without its property's getter")?;
                library_member_signature(getter, MemberRole::Setter)?
            }
            _ => library_member_signature(&fact, role)?,
        };
        let owner = library_member_owner(&fact).ok_or("a library member without its classifier")?;
        let member = self.runtime.objects.interfaces.library(
            self.module,
            owner,
            &identity,
            signature.clone(),
        )?;
        self.runtime.builtin_member(
            self.module,
            member,
            &signature,
            &fact.builtin_implementations,
        )?;
        if role == MemberRole::Getter && property.is_some_and(is_reference_name) {
            self.runtime.reference_name_member(self.module, member)?;
        }
        if let Some(step) = super::array_lists::iterator_step(&fact) {
            let function = match step {
                super::array_lists::IteratorStep::HasNext => self.runtime.array_lists.has_next,
                super::array_lists::IteratorStep::Next => self.runtime.array_lists.next,
            };
            self.runtime.objects.interfaces.implement_representation(
                member,
                self.runtime.array_lists.iterator,
                function,
            )?;
        }
        self.value(receiver, objects::any())?;
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
        }
        let stub = self.runtime.objects.interfaces.stub(self.module, member);
        self.code.call(stub);
        match self.type_of(id)? {
            Some(ty) => self.coerce_emitted_value(result, ty),
            None => {
                self.code.unreachable();
                Ok(())
            }
        }
    }
}
