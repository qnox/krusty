//! Constructions and member calls of `kotlin.collections.ArrayList`, which the runtime lays out
//! (`crate::array_lists`).
//!
//! The selected library declaration says which member a call is: its declaring classifier and its
//! name, as the provider froze them. A member the runtime does not realize declines the file.

use krusty::backend::BackendCallableFact;
use krusty::fir::ExternalCallableId;
use krusty::types::{SemanticCallableOwner, Ty, TypeName};

use super::*;

/// Whether `classifier` is `kotlin.collections.ArrayList`.
pub(in crate::codegen) fn is_array_list(classifier: TypeName) -> bool {
    crate::array_lists::lays_out(classifier)
}

/// Whether the selected declaration `fact` is declared by `ArrayList`.
pub(in crate::codegen) fn declared_by_array_list(fact: &BackendCallableFact) -> bool {
    matches!(
        fact.declaration_owner,
        Some(SemanticCallableOwner::Classifier(owner)) if is_array_list(owner)
    )
}

/// Whether `fact` is a collection operation the runtime realizes on an `ArrayList` receiver
/// (`crate::array_lists::list_operation`).
pub(in crate::codegen) fn realized_array_list_member(fact: &BackendCallableFact) -> bool {
    crate::array_lists::list_operation(&fact.name, &fact.params).is_ok()
}

/// Whether the checked type of `receiver` is `kotlin.collections.ArrayList`.
pub(in crate::codegen) fn array_list_receiver(ir: &krusty::ir::IrFile, receiver: u32) -> bool {
    ir.checked_type(receiver)
        .and_then(|ty| ty.kotlin_class_internal())
        .is_some_and(is_array_list)
}

/// The step the runtime list iterator realizes for `fact`, a member of `Iterator`, `ListIterator`
/// or their mutable forms; `None` for any other member.
pub(in crate::codegen) fn iterator_step(fact: &BackendCallableFact) -> Option<IteratorStep> {
    let Some(SemanticCallableOwner::Classifier(owner)) = fact.declaration_owner else {
        return None;
    };
    if ![
        "kotlin/collections/Iterator",
        "kotlin/collections/MutableIterator",
        "kotlin/collections/ListIterator",
        "kotlin/collections/MutableListIterator",
    ]
    .into_iter()
    .any(|iterator| owner.matches(iterator))
    {
        return None;
    }
    match (fact.name.as_str(), fact.params.len()) {
        ("hasNext", 0) => Some(IteratorStep::HasNext),
        ("next", 0) => Some(IteratorStep::Next),
        ("hasPrevious", 0) => Some(IteratorStep::HasPrevious),
        ("previous", 0) => Some(IteratorStep::Previous),
        ("nextIndex", 0) => Some(IteratorStep::NextIndex),
        ("previousIndex", 0) => Some(IteratorStep::PreviousIndex),
        _ => None,
    }
}

/// One step of the runtime iterator.
#[derive(Clone, Copy)]
pub(in crate::codegen) enum IteratorStep {
    HasNext,
    Next,
    HasPrevious,
    Previous,
    NextIndex,
    PreviousIndex,
}

impl Body<'_> {
    /// `ArrayList(...)` through the selected library constructor `target`.
    pub(super) fn array_list_construction(
        &mut self,
        target: ExternalCallableId,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let fact = self
            .callables
            .callable(target)
            .ok_or_else(|| "an ArrayList constructor without its declaration".to_string())?;
        if fact.kind != krusty::libraries::ExternalCallableKind::Constructor
            || !declared_by_array_list(fact)
            || fact.params.len() != args.len()
        {
            return Err("an ArrayList construction disagreeing with its constructor".into());
        }
        self.record_collection_arguments()?;
        let new = self.runtime.array_lists.new;
        let add_all = self
            .runtime
            .array_lists
            .member(crate::array_lists::ListOperation::AddAll)?
            .function;
        match args {
            [] => {
                self.code.i32_const(10).call(new);
            }
            [capacity] if fact.params.first() == Some(&Ty::Int) => {
                self.value(*capacity, Ty::Int)?;
                self.code.call(new);
            }
            [elements] => {
                // `ArrayList(elements)` is a new list holding the elements, so the list stays
                // the construction's value after `addAll` answers whether it changed.
                let list = self.local(REFERENCE);
                self.code.i32_const(0).call(new).local_tee(list);
                self.value(*elements, fact.params[0])?;
                self.code.call(add_all).drop_().local_get(list);
            }
            _ => return Err("an ArrayList constructor the runtime does not realize".into()),
        }
        Ok(())
    }

    /// A call of the selected library member `target` of `ArrayList` on `receiver`.
    pub(super) fn array_list_call(
        &mut self,
        id: u32,
        target: ExternalCallableId,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let fact = self
            .callables
            .callable(target)
            .ok_or_else(|| "an ArrayList member without its declaration".to_string())?;
        if fact.params.len() != args.len()
            || !(declared_by_array_list(fact) || realized_array_list_member(fact))
        {
            return Err("an ArrayList call disagreeing with its member".into());
        }
        let operation = crate::array_lists::list_operation(&fact.name, &fact.params)?;
        self.record_collection_arguments()?;
        let function = self.runtime.array_lists.member(operation)?.function;
        self.value(receiver, Ty::obj("kotlin/collections/ArrayList"))?;
        for (&arg, &param) in args.iter().zip(fact.params.as_slice()) {
            self.argument(arg, param)?;
        }
        if let Some(bound) = operation.index_bound() {
            // The index the member takes, its only argument, must stay within `bound`.
            let size = self.runtime.array_lists.size;
            let (list, index) = (self.local(REFERENCE), self.local(ValType::I32));
            // `set` and an insertion take the element after the index: set it aside while the
            // index is checked.
            let element = matches!(
                operation,
                crate::array_lists::ListOperation::Set | crate::array_lists::ListOperation::AddAt
            )
            .then(|| self.local(REFERENCE));
            if let Some(element) = element {
                self.code.local_set(element);
            }
            self.code
                .local_set(index)
                .local_tee(list)
                .local_get(index)
                .local_get(list)
                .call(size);
            if bound == crate::array_lists::IndexBound::AtMostSize {
                self.code.i32_const(1).op(crate::runtime::ADD32);
            }
            self.throw_unless_index_below()?;
            self.code.local_get(index);
            if let Some(element) = element {
                self.code.local_get(element);
            }
        }
        self.code.call(function);
        // An element is carried as a reference, whatever the list's element type.
        let result = fact.declared_ret.unwrap_or(fact.ret);
        match self.type_of(id)? {
            Some(ty) => self.coerce_emitted_value(result, ty),
            None => {
                self.code.unreachable();
                Ok(())
            }
        }
    }

    /// A read of the library property `target` of `ArrayList` on `receiver`.
    pub(super) fn array_list_property(
        &mut self,
        id: u32,
        target: krusty::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let getter = self
            .callables
            .property(target)
            .map(|property| property.getter)
            .ok_or_else(|| "an ArrayList property without its declaration".to_string())?;
        self.array_list_call(id, getter, receiver, &[])
    }

    /// Whether `target` is a property declared by `ArrayList`.
    pub(super) fn is_array_list_property(&self, target: krusty::fir::ExternalPropertyId) -> bool {
        self.callables
            .property(target)
            .and_then(|property| self.callables.callable(property.getter))
            .is_some_and(declared_by_array_list)
    }
}

impl Body<'_> {
    /// Record the checked collection members the runtime list reads a collection argument
    /// through (`crate::collection_arguments`), which defines the list operations reading one,
    /// then what the list and the builtin classes run for each member. Anything reaching a
    /// runtime list's member records them first.
    pub(super) fn record_collection_arguments(&mut self) -> Result<(), Unsupported> {
        if self.runtime.collection_arguments.has_members() {
            return Ok(());
        }
        let protocol = self.callables.collection_protocol().ok_or(
            "a runtime `ArrayList` without the checked collection members it reads an argument \
             through",
        )?;
        let targets = [
            (protocol.iterator, (1, 1)),
            (protocol.has_next, (1, 1)),
            (protocol.next, (1, 1)),
            (protocol.contains, (2, 1)),
        ];
        let mut facts = Vec::with_capacity(targets.len());
        let mut members = Vec::with_capacity(targets.len());
        for (target, arity) in targets {
            let (fact, identity, member) = self.collection_member(target, arity)?;
            facts.push((fact, identity));
            members.push(member);
        }
        self.runtime.record_collection_members(
            self.module,
            self.classifiers,
            crate::collection_arguments::CollectionMembers {
                iterator: members[0],
                has_next: members[1],
                next: members[2],
                contains: members[3],
            },
        )?;
        let role = super::library_members::MemberRole::Function;
        for (fact, identity) in facts {
            let name = fact.name.clone();
            self.library_member_of(&fact, identity, role, &name)?;
        }
        Ok(())
    }

    /// The library member `target`, carried as `arity` wasm parameters (the receiver first) and
    /// results, as the runtime list calls it, with its declaration and KLIB identity.
    fn collection_member(
        &mut self,
        target: ExternalCallableId,
        (params, results): (usize, usize),
    ) -> Result<
        (
            BackendCallableFact,
            Vec<u8>,
            crate::interface_dispatch::MemberRef,
        ),
        Unsupported,
    > {
        let fact = self
            .callables
            .callable(target)
            .ok_or("a collection member without its declaration")?
            .clone();
        let role = super::library_members::MemberRole::Function;
        let signature = super::library_members::library_member_signature(&fact, role)?;
        if signature.0.len() != params || signature.1.len() != results {
            return Err(format!(
                "the collection member `{}`, carried otherwise than the runtime list reads it",
                fact.name
            ));
        }
        let identity = super::library_members::library_member_identity(&fact, role)
            .ok_or("a collection member without its identity")?;
        let owner = super::library_members::library_member_owner(&fact)
            .ok_or("a collection member without its classifier")?;
        let member =
            self.runtime
                .objects
                .interfaces
                .library(self.module, owner, &identity, signature)?;
        Ok((fact, identity, member))
    }
}
