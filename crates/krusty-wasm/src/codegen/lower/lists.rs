//! `ArrayList` reads, writes, and packed or empty mutable construction.
//!
//! Those members are not ordinary emitted roots, and the factory's KLIB body reaches a non-public
//! helper. The runtime stores the list. `get` and `removeAt` share a type shape; the frozen
//! declaration spelling is what distinguishes them, as the native realization of the same members
//! does.

use krusty::backend::BackendCallableFact;
use krusty::libraries::{CompilerIntrinsic, ListStorageConstruction, RuntimeExceptionKind};
use krusty::types::SemanticCallableOwner;

use crate::array_objects::STORAGE_FIELD;

use super::*;

const ELEMENTS: u32 = 1;
const SIZE: u32 = 2;
const I32_GE_U: u8 = 0x4f;

enum RealizedList {
    Get,
    Set,
    MutablePacked,
    MutableEmpty,
}

/// An `ArrayList` member this backend emits itself, so the call has no dependency body to link.
pub(in crate::codegen) fn realized_without_dependency_body(fact: &BackendCallableFact) -> bool {
    array_list_member(fact).is_some()
}

fn array_list_member(fact: &BackendCallableFact) -> Option<RealizedList> {
    let SemanticCallableOwner::Classifier(owner) = fact.declaration_owner? else {
        return None;
    };
    if !owner.matches("kotlin/collections/ArrayList") || !fact.is_member_function() {
        return None;
    }
    match fact.name.as_str() {
        "get" if fact.params.as_slice() == [Ty::Int] && fact.ret.is_reference() => {
            Some(RealizedList::Get)
        }
        "set"
            if matches!(fact.params.as_slice(), [Ty::Int, element] if element.is_reference())
                && fact.ret.is_reference() =>
        {
            Some(RealizedList::Set)
        }
        _ => None,
    }
}

/// `ArrayList()` with no arguments. An empty `arrayListOf` is that constructor once its inline
/// body is in the caller, and the class is not one this file declares.
pub(super) fn is_empty_array_list_constructor(fact: &BackendCallableFact) -> bool {
    let Some(SemanticCallableOwner::Classifier(owner)) = fact.declaration_owner else {
        return false;
    };
    fact.kind == krusty::libraries::ExternalCallableKind::Constructor
        && owner.matches("kotlin/collections/ArrayList")
        && fact.params.is_empty()
}

fn realized_list(fact: &BackendCallableFact) -> Option<RealizedList> {
    match fact.compiler_operation() {
        Some(CompilerIntrinsic::ListStorageConstruction(
            ListStorageConstruction::MutablePacked,
        )) => Some(RealizedList::MutablePacked),
        Some(CompilerIntrinsic::ListStorageConstruction(ListStorageConstruction::MutableEmpty)) => {
            Some(RealizedList::MutableEmpty)
        }
        _ => array_list_member(fact),
    }
}

impl Body<'_> {
    pub(super) fn is_realized_list_call(&self, callee: &Callee) -> bool {
        let Callee::External {
            target,
            defaults,
            extension_receiver_parameter: None,
            ..
        } = callee
        else {
            return false;
        };
        defaults.is_empty()
            && self
                .callables
                .callable(*target)
                .is_some_and(|fact| realized_list(fact).is_some())
    }

    pub(super) fn emit_realized_list_call(
        &mut self,
        callee: &Callee,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let Callee::External { target, ret, .. } = callee else {
            return Err("a realized list call that is not a dependency".into());
        };
        let operation = self
            .callables
            .callable(*target)
            .and_then(realized_list)
            .ok_or("a realized list call without its frozen declaration")?;
        match operation {
            RealizedList::Get => {
                let (Some(receiver), [index]) = (dispatch_receiver, args) else {
                    return Err("an `ArrayList` read with malformed operands".into());
                };
                self.array_list_get(receiver, *index, *ret)
            }
            RealizedList::Set => {
                let (Some(receiver), [index, value]) = (dispatch_receiver, args) else {
                    return Err("an `ArrayList` write with malformed operands".into());
                };
                self.array_list_set(receiver, *index, *value, *ret)
            }
            RealizedList::MutablePacked => {
                if dispatch_receiver.is_some() {
                    return Err("a mutable list factory with a receiver".into());
                }
                let [elements] = args else {
                    return Err("a mutable list built from malformed operands".into());
                };
                self.mutable_packed_list(*elements)
            }
            RealizedList::MutableEmpty => {
                if dispatch_receiver.is_some() || !args.is_empty() {
                    return Err("an empty mutable list built from malformed operands".into());
                }
                self.mutable_empty_list()
            }
        }
    }

    fn array_list_get(&mut self, receiver: u32, index: u32, ret: Ty) -> Result<(), Unsupported> {
        let (list, offset) = self.list_operands(receiver, index)?;
        self.check_list_index(list, offset)?;
        self.list_element(list, offset);
        self.coerce_emitted_value(any_element(), ret)
    }

    fn array_list_set(
        &mut self,
        receiver: u32,
        index: u32,
        value: u32,
        ret: Ty,
    ) -> Result<(), Unsupported> {
        let (list, offset) = self.list_operands(receiver, index)?;
        let stored = self.local(REFERENCE);
        self.value(value, any_element())?;
        self.code.local_set(stored);
        self.check_list_index(list, offset)?;
        let previous = self.local(REFERENCE);
        self.list_element(list, offset);
        self.code.local_set(previous);
        let elements = self.runtime.lists.elements;
        let structure = self.runtime.lists.structure;
        self.code
            .local_get(list)
            .struct_get(structure, ELEMENTS)
            .local_get(offset)
            .local_get(stored)
            .array_set(elements);
        self.code.local_get(previous);
        self.coerce_emitted_value(any_element(), ret)
    }

    /// `arrayListOf(...)`: a growable list holding a copy of the packed array.
    fn mutable_packed_list(&mut self, elements: u32) -> Result<(), Unsupported> {
        let array_ty = self
            .type_of(elements)?
            .ok_or_else(|| "a mutable list built from an operand without a type".to_string())?;
        if !array_ty.non_null().is_reference_array() {
            return Err("a mutable list built from a non-reference array".into());
        }
        let (form, _) = self
            .runtime
            .array_form(self.module, array_ty)
            .ok_or_else(|| {
                "a mutable list built from an array without reference storage".to_string()
            })?;
        let storage = self.runtime.lists.elements;
        if form.storage != storage {
            return Err("a mutable list built from another array storage".into());
        }
        let source = self.local(ValType::reference(storage));
        let length = self.local(ValType::I32);
        let destination = self.local(ValType::reference(storage));
        self.value(elements, array_ty)?;
        self.code
            .ref_cast(false, HeapType::Concrete(form.object))
            .struct_get(form.object, STORAGE_FIELD)
            .local_set(source);
        self.code.local_get(source).array_len().local_set(length);
        self.code
            .local_get(length)
            .array_new_default(storage)
            .local_set(destination);
        self.code
            .local_get(destination)
            .i32_const(0)
            .local_get(source)
            .i32_const(0)
            .local_get(length)
            .array_copy(storage, storage);
        let structure = self.runtime.lists.structure;
        self.code
            .global_get(self.runtime.lists.table)
            .local_get(destination)
            .local_get(length)
            .struct_new(structure);
        Ok(())
    }

    pub(super) fn mutable_empty_list(&mut self) -> Result<(), Unsupported> {
        let storage = self.runtime.lists.elements;
        let structure = self.runtime.lists.structure;
        self.code
            .global_get(self.runtime.lists.table)
            .i32_const(0)
            .array_new_default(storage)
            .i32_const(0)
            .struct_new(structure);
        Ok(())
    }

    fn list_operands(&mut self, receiver: u32, index: u32) -> Result<(u32, u32), Unsupported> {
        let receiver_ty = self
            .type_of(receiver)?
            .ok_or_else(|| "an `ArrayList` receiver without a checked type".to_string())?;
        let structure = self.runtime.lists.structure;
        let list = self.local(ValType::reference(structure));
        let offset = self.local(ValType::I32);
        self.value(receiver, receiver_ty)?;
        self.code
            .ref_cast(false, HeapType::Concrete(structure))
            .local_set(list);
        self.value(index, Ty::Int)?;
        self.code.local_set(offset);
        Ok((list, offset))
    }

    fn list_element(&mut self, list: u32, offset: u32) {
        let structure = self.runtime.lists.structure;
        self.code
            .local_get(list)
            .struct_get(structure, ELEMENTS)
            .local_get(offset)
            .array_get(self.runtime.lists.elements);
    }

    fn check_list_index(&mut self, list: u32, offset: u32) -> Result<(), Unsupported> {
        if !self.array_range_checks {
            return Ok(());
        }
        let exception = self.runtime.exception_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IndexOutOfBounds.classifier(),
        )?;
        let structure = self.runtime.lists.structure;
        self.code
            .local_get(offset)
            .local_get(list)
            .struct_get(structure, SIZE)
            .op(I32_GE_U)
            .if_(BlockType::Empty);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            exception,
            None,
            self.runtime.string,
        )?;
        self.code.throw(self.runtime.exceptions.tag).end();
        Ok(())
    }
}

fn any_element() -> Ty {
    Ty::nullable(Ty::obj_name(krusty::types::wk::any()))
}
