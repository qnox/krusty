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
    classifier.matches("kotlin/collections/ArrayList")
}

/// Whether the selected declaration `fact` is declared by `ArrayList`.
pub(in crate::codegen) fn declared_by_array_list(fact: &BackendCallableFact) -> bool {
    matches!(
        fact.declaration_owner,
        Some(SemanticCallableOwner::Classifier(owner)) if is_array_list(owner)
    )
}

/// Whether `fact` is a collection operation the runtime realizes on an `ArrayList` receiver.
///
/// `ArrayList` inherits several of these from `AbstractMutableList` and `AbstractCollection`.
/// The selected declaration then names the superclass, while the receiver is still the list the
/// runtime laid out.
pub(in crate::codegen) fn realized_array_list_member(fact: &BackendCallableFact) -> bool {
    match (fact.name.as_str(), fact.params.as_slice()) {
        ("add", [_])
        | ("addAll", [_])
        | ("<get-size>" | "getSize" | "size", [])
        | ("isEmpty", [])
        | ("contains", [_])
        | ("containsAll", [_])
        | ("removeAll", [_])
        | ("retainAll", [_])
        | ("clear", [])
        | ("iterator", []) => true,
        ("get", [Ty::Int]) => true,
        ("remove", [param]) => *param != Ty::Int,
        _ => false,
    }
}

/// Whether the checked type of `receiver` is `kotlin.collections.ArrayList`.
pub(in crate::codegen) fn array_list_receiver(ir: &krusty::ir::IrFile, receiver: u32) -> bool {
    ir.checked_type(receiver)
        .and_then(|ty| ty.kotlin_class_internal())
        .is_some_and(is_array_list)
}

/// Whether `fact` is `Iterator.hasNext`, `Iterator.next`, or the same members redeclared by
/// `MutableIterator`.
pub(in crate::codegen) fn iterator_step(fact: &BackendCallableFact) -> Option<IteratorStep> {
    let Some(SemanticCallableOwner::Classifier(owner)) = fact.declaration_owner else {
        return None;
    };
    if !owner.matches("kotlin/collections/Iterator")
        && !owner.matches("kotlin/collections/MutableIterator")
    {
        return None;
    }
    match (fact.name.as_str(), fact.params.len()) {
        ("hasNext", 0) => Some(IteratorStep::HasNext),
        ("next", 0) => Some(IteratorStep::Next),
        _ => None,
    }
}

/// One step of the runtime iterator.
#[derive(Clone, Copy)]
pub(in crate::codegen) enum IteratorStep {
    HasNext,
    Next,
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
        let new = self.runtime.array_lists.new;
        let add_all = self.runtime.array_lists.add_all;
        match args {
            [] => {
                self.code.i32_const(10).call(new);
            }
            [capacity] if fact.params.first() == Some(&Ty::Int) => {
                self.value(*capacity, Ty::Int)?;
                self.code.call(new);
            }
            [elements] => {
                self.code.i32_const(0).call(new);
                self.value(*elements, fact.params[0])?;
                self.code.call(add_all).drop_();
            }
            _ => return Err("an ArrayList constructor the runtime does not realize".into()),
        }
        Ok(())
    }

    /// A call of the selected library member `target` of `ArrayList` on `receiver`.
    pub(super) fn array_list_call(
        &mut self,
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
        let lists = &self.runtime.array_lists;
        let function = match (fact.name.as_str(), fact.params.as_slice()) {
            ("add", [_]) => lists.add,
            ("add", [Ty::Int, _]) => {
                return Err("an ArrayList insertion at an index".into());
            }
            ("addAll", [_]) => lists.add_all,
            ("<get-size>", []) | ("getSize", []) => lists.size,
            ("isEmpty", []) => lists.is_empty,
            ("get", [Ty::Int]) => lists.get,
            ("contains", [_]) => lists.contains,
            ("containsAll", [_]) => lists.contains_all,
            ("remove", [Ty::Int]) => {
                return Err("an ArrayList removal at an index".into());
            }
            ("remove", [_]) => lists.remove,
            ("removeAll", [_]) => lists.remove_all,
            ("retainAll", [_]) => lists.retain_all,
            ("clear", []) => lists.clear,
            ("iterator", []) => lists.iterator_new,
            _ => {
                return Err(format!(
                    "the ArrayList member `{}`, which the runtime does not realize",
                    fact.name
                ));
            }
        };
        self.value(receiver, Ty::obj("kotlin/collections/ArrayList"))?;
        for (&arg, &param) in args.iter().zip(fact.params.as_slice()) {
            self.argument(arg, param)?;
        }
        self.code.call(function);
        Ok(())
    }

    /// A read of the library property `target` of `ArrayList` on `receiver`.
    pub(super) fn array_list_property(
        &mut self,
        target: krusty::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let getter = self
            .callables
            .property(target)
            .map(|property| property.getter)
            .ok_or_else(|| "an ArrayList property without its declaration".to_string())?;
        self.array_list_call(getter, receiver, &[])
    }

    /// Whether `target` is a property declared by `ArrayList`.
    pub(super) fn is_array_list_property(&self, target: krusty::fir::ExternalPropertyId) -> bool {
        self.callables
            .property(target)
            .and_then(|property| self.callables.callable(property.getter))
            .is_some_and(declared_by_array_list)
    }
}
