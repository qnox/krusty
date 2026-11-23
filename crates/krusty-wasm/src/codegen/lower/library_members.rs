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

use crate::callable_references::{AccessorShape, PropertyAccess};
use krusty::backend::interface_identity::property_member_identity;
use krusty::backend::{BackendCallableFact, BackendPropertyFact, CheckedBackendCallables};
use krusty::libraries::ExternalCallableKind;
use krusty::libraries::MemberRealization;
use krusty::types::{SemanticCallRole, SemanticCallableOwner, TypeName};

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
            // The getter's declared parameters are its extension receiver, if it has one, which
            // the setter takes before the value.
            let (value, result) = accessor_types(result, true);
            member_signature(&[params, value.as_slice()].concat(), result)
        }
    }
}

/// Whether a call of `fact` with a dispatch receiver at `expression` goes through its library
/// member: a member of a library classifier that dispatches on its receiver, with no `published`
/// body to call (an interface's member dispatches whatever its default publishes) and no compiler
/// operation realizing it (`KClass.simpleName`), which an interface's classes or the builtin classes below it implement.
pub(in crate::codegen) fn dispatches_through_library_member(
    ir: &IrFile,
    callables: &CheckedBackendCallables,
    expression: u32,
    fact: &BackendCallableFact,
    published: bool,
) -> bool {
    // A published body is the one a call reaches directly, except an interface's default: the
    // receiver's class may override it, and a class that inherits it reaches the body from its
    // own record.
    (!published || fact.owner_is_interface)
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

/// The library member a call goes through: its KLIB identity, how the call reaches it, and the
/// Kotlin name of the declaration its overrides share.
struct LibraryMember {
    identity: Vec<u8>,
    role: MemberRole,
    name: String,
    /// Whether the member is `KCallable.name`, which every callable-reference object answers.
    reference_name: bool,
    /// The published body an interface declares for the member: the implementation of every
    /// class that does not override it.
    interface_default: Option<u32>,
}

/// Whether `property` is `KCallable.name`, which every callable-reference object implements
/// (`crate::callable_references`).
fn is_reference_name(property: &BackendPropertyFact) -> bool {
    &*property.name == "name" && property.owner.matches("kotlin/reflect/KCallable")
}

/// Whether `fact` is a member of a library interface that dispatches on its receiver, which a
/// class implementing the interface may override.
pub(in crate::codegen) fn is_dispatched_interface_member(fact: &BackendCallableFact) -> bool {
    fact.owner_is_interface
        && fact.kind == ExternalCallableKind::Member
        && fact.member_realization == MemberRealization::Dispatch
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
        // A function's overrides are named by the function, an accessor's by its property.
        let (target, receiver, args, (property, role), generic_accessor, member_extension) =
            match node {
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
                        false,
                        false,
                    )
                }
                IrExpr::Checked(operation) => {
                    let access = self.callables.dependency_accessor(operation)?;
                    let receiver = access.receiver?;
                    // A generic getter's instantiated virtual frame is checked below; a setter goes
                    // through the published-class path.
                    let generic_accessor = !access.writes()
                        && (!access.substitutions().is_empty()
                            || access
                                .callable
                                .generic_sig
                                .as_ref()
                                .is_some_and(|signature| !signature.formals.is_empty()));
                    if !access.dispatches() {
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
                        generic_accessor,
                        access.callable.is_member_extension(),
                    )
                }
                _ => return None,
            };
        let fact = self.callables.callable(target)?;
        if generic_accessor
            && (member_extension
                || (matches!(fact.kind, ExternalCallableKind::Member)
                    && fact.member_realization == MemberRealization::Dispatch
                    && fact.compiler_operation().is_none()))
        {
            // Whether an export exists must not decide virtual semantics. The direct dependency
            // body path below cannot instantiate this generic accessor's virtual frame, so do not
            // let it bypass a possible override.
            return Some(Err(
                "a generic dependency member getter whose instantiated virtual interface call is unsupported"
                    .into(),
            ));
        }
        if member_extension {
            return None;
        }
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
        // A library member call reads a property only through its checked property read.
        if role == MemberRole::Setter
            || (property.is_some()
                && !matches!(
                    node,
                    IrExpr::Checked(operation) if matches!(
                        &**operation,
                        krusty::ir::IrCheckedOperation::ExternalPropertyRead { .. }
                    )
                ))
        {
            return None;
        }
        let published = self.calls.external(target).is_some();
        let dispatches =
            dispatches_through_library_member(self.ir, self.callables, id, fact, published);
        if !dispatches {
            return None;
        }
        // A function's identity names a function, and an accessor's a property.
        let identity = library_member_identity(fact, role)?;
        let name = match property {
            Some(property) => property.name.to_string(),
            None => fact.name.clone(),
        };
        Some(self.library_member_call(
            id,
            fact.clone(),
            LibraryMember {
                identity,
                role,
                name,
                reference_name: property.is_some_and(is_reference_name),
                interface_default: self.calls.interface_default(target),
            },
            receiver,
            args,
        ))
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
            self.unit_argument(parameter);
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

    /// A call through the library member `fact` declares, the getter of `property` when there is
    /// one.
    fn library_member_call(
        &mut self,
        id: u32,
        fact: BackendCallableFact,
        member: LibraryMember,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let LibraryMember {
            identity,
            role,
            name,
            reference_name,
            interface_default,
        } = member;
        let params = fact.declared_params.as_deref().unwrap_or(&fact.params);
        let result = fact.declared_ret.unwrap_or(fact.ret);
        if params.len() != args.len() {
            return Err("a call whose arguments do not match its parameters".into());
        }
        // The builder's `append(CharSequence?)` reads the appended sequence through the checked
        // `CharSequence` members on a target that copies its units.
        if fact.builtin_implementations.iter().any(|implementation| {
            super::string_builders::is_string_builder_type(implementation.receiver)
        }) {
            self.record_char_sequence_reads(false)?;
        }
        let member = self.library_member_of(&fact, identity, role, &name)?;
        if let Some(function) = interface_default {
            self.runtime
                .objects
                .interfaces
                .interface_default(member, function)?;
        }
        if reference_name {
            self.runtime.reference_name_member(self.module, member)?;
        }
        let accessor = match fact.semantic_role {
            Some(SemanticCallRole::KotlinPropertyReferenceGet(arity))
                if usize::from(arity) == args.len() =>
            {
                Some(PropertyAccess::Read)
            }
            Some(SemanticCallRole::KotlinPropertyReferenceSet(arity))
                if usize::from(arity) + 1 == args.len() =>
            {
                Some(PropertyAccess::Write)
            }
            _ => None,
        };
        if let Some(access) = accessor {
            self.runtime.property_reference_accessor_member(
                self.module,
                member,
                AccessorShape {
                    access,
                    operands: args.len(),
                },
            )?;
        }
        self.runtime
            .null_receiver_throws(self.module, self.classifiers)?;
        self.value(receiver, objects::any())?;
        for (&arg, &param) in args.iter().zip(params) {
            self.argument(arg, param)?;
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

impl Body<'_> {
    /// The library member `fact` declares in `role`, named by its KLIB `identity`, with what the
    /// builtin classes and the runtime list's iterator run for it recorded: the member a call
    /// through it, or the runtime reading a collection or a sequence through it, goes to.
    pub(super) fn library_member_of(
        &mut self,
        fact: &BackendCallableFact,
        identity: Vec<u8>,
        role: MemberRole,
        name: &str,
    ) -> Result<crate::interface_dispatch::MemberRef, Unsupported> {
        // The runtime list's operations reading a collection argument exist once the members they
        // read it through are recorded.
        if fact.builtin_implementations.iter().any(|implementation| {
            implementation
                .receiver
                .obj_internal()
                .is_some_and(crate::array_lists::lays_out)
        }) {
            self.record_collection_arguments()?;
        }
        let signature = library_member_signature(fact, role)?;
        let owner = library_member_owner(fact).ok_or("a library member without its classifier")?;
        let member = self.runtime.objects.interfaces.library(
            self.module,
            owner,
            &identity,
            signature.clone(),
        )?;
        self.runtime.builtin_member(
            self.module,
            self.classifiers,
            member,
            crate::builtin_members::LibraryMemberDeclaration {
                name,
                params: fact.declared_params.as_deref().unwrap_or(&fact.params),
                implementations: &fact.builtin_implementations,
            },
            &signature,
        )?;
        if let Some(step) = super::array_lists::iterator_step(fact) {
            use super::array_lists::IteratorStep;
            use crate::list_iterator_ends::EndedStep;
            let lists = &self.runtime.array_lists;
            let function = match step {
                IteratorStep::HasNext => lists.has_next,
                IteratorStep::HasPrevious => lists.has_previous,
                IteratorStep::NextIndex => lists.next_index,
                IteratorStep::PreviousIndex => lists.previous_index,
                IteratorStep::Next | IteratorStep::Previous => {
                    let ended = if matches!(step, IteratorStep::Next) {
                        EndedStep::Next
                    } else {
                        EndedStep::Previous
                    };
                    self.runtime
                        .checked_list_iterator_step(self.module, self.classifiers, ended)?
                }
            };
            self.runtime.objects.interfaces.implement_representation(
                member,
                self.runtime.array_lists.iterator,
                function,
            )?;
        }
        Ok(member)
    }
}
