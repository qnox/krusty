//! Allocation of a runtime `HashMap` or `LinkedHashMap`, and the entry operations it answers.

use krusty::libraries::HashMapOperation;
use krusty::types::Ty;

use super::super::super::interface_dispatch::MemberRef;
use super::super::super::maps::MapSlot;
use super::*;

impl Body<'_> {
    /// `HashMap(...)` or `LinkedHashMap(...)`. Capacity and load factor are hints; a map copied
    /// from another map is not this constructor. The arrays start empty and grow on `put`.
    pub(super) fn hash_map_construction(&mut self, args: &[u32]) -> Result<(), Unsupported> {
        for &arg in args {
            let ty = self
                .type_of(arg)?
                .ok_or("a map constructor argument without a type")?;
            self.value(arg, ty)?;
            if carrier(ty)?.is_some() {
                self.code.drop_();
            }
        }
        self.runtime.maps.construct(&mut self.code);
        Ok(())
    }

    /// A direct call of `HashMap` or `LinkedHashMap`'s `get`, `containsKey`, or `put`.
    pub(super) fn hash_map_entry(
        &mut self,
        operation: HashMapOperation,
        receiver: u32,
        args: &[u32],
        params: &[Ty],
        ret: Ty,
    ) -> Result<(), Unsupported> {
        if params.len() != args.len() {
            return Err("a map entry operation with malformed operands".to_string());
        }
        self.value(receiver, objects::any())?;
        for (&arg, &param) in args.iter().zip(params) {
            self.value(arg, param)?;
        }
        self.code
            .call(self.runtime.maps.function(map_slot(operation)));
        let produced = match operation {
            HashMapOperation::ContainsKey => Ty::Boolean,
            HashMapOperation::Get | HashMapOperation::Put => Ty::nullable(objects::any()),
        };
        self.coerce_emitted_value(produced, ret)
    }

    /// `Map.get`, `Map.containsKey`, or `MutableMap.put` on the runtime map.
    pub(super) fn implement_hash_map(
        &mut self,
        member: MemberRef,
        operation: HashMapOperation,
    ) -> Result<(), Unsupported> {
        let function = self.runtime.maps.function(map_slot(operation));
        let object = self.runtime.maps.object;
        self.runtime
            .objects
            .interfaces
            .implement_representation(member, object, function)?;
        Ok(())
    }
}

fn map_slot(operation: HashMapOperation) -> MapSlot {
    match operation {
        HashMapOperation::Get => MapSlot::Get,
        HashMapOperation::ContainsKey => MapSlot::ContainsKey,
        HashMapOperation::Put => MapSlot::Put,
    }
}
