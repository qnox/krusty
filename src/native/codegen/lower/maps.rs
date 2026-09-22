//! `mapOf(...)` / `setOf(...)` and what a program does with the map or set it answers.
//!
//! A map is two growable lists side by side — its keys in insertion order and the values beside
//! them — and a set is the same object with no values, which is what Kotlin's own `LinkedHashSet`
//! is. The runtime answers their members (`src/native/runtime/krusty_rt.c`, the `maps and sets`
//! section), so `equals`, `hashCode` and `toString` agree with what the Kotlin declarations say.
//!
//! Which call reaches the runtime is decided by the RECEIVER's type, never by the owner declaring
//! the member — the position `lists.rs` already takes, and for the same reason: `get` is declared
//! on `Map`, which a user class may implement, and keying on the owner would send a user's object
//! into the runtime. A file that declares such an implementation is declined at every member asked
//! of a type it implements itself, before this is reached.

use super::*;

/// Whether a type is one of the runtime's maps, sets, or map entries. Which spellings each covers
/// — and the normalization a jar provider's `java.util` names need — is
/// [`intrinsics::is_map_type`]'s to know; this only asks.
fn is_map(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(super::super::super::intrinsics::is_map_type)
}

fn is_set(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(super::super::super::intrinsics::is_set_type)
}

fn is_map_entry(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(super::super::super::intrinsics::is_map_entry_type)
}

/// The runtime function answering one member of a map, with the types it is carried at.
///
/// `size` is an `Int` the generator must not box to ask about, which is why each signature is
/// spelled out rather than taken as all-references.
fn map_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        // `Map.size` is a Kotlin property over a Java method, so the provider may present the
        // getter under either spelling; both name the same question.
        ("getSize" | "size", 0) => ("kt_map_size", vec![any()], Ty::Int),
        ("isEmpty", 0) => ("kt_map_is_empty", vec![any()], Ty::Boolean),
        ("get", 1) => ("kt_map_get", vec![any(), any()], any()),
        ("getOrDefault", 2) => ("kt_map_get_or_default", vec![any(), any(), any()], any()),
        ("containsKey", 1) => ("kt_map_contains_key", vec![any(), any()], Ty::Boolean),
        ("containsValue", 1) => ("kt_map_contains_value", vec![any(), any()], Ty::Boolean),
        ("keys" | "getKeys", 0) => ("kt_map_keys", vec![any()], any()),
        ("values" | "getValues", 0) => ("kt_map_values", vec![any()], any()),
        ("entries" | "getEntries", 0) => ("kt_map_entries", vec![any()], any()),
        // The mutating half. `put` answers the value that was there; `set` is `m[k] = v` and
        // answers `Unit`, so it is its own entry point rather than a result a caller has to
        // remember to drop.
        ("put", 2) => ("kt_map_put", vec![any(), any(), any()], any()),
        ("set", 2) => ("kt_map_set", vec![any(), any(), any()], Ty::Unit),
        ("remove", 1) => ("kt_map_remove", vec![any(), any()], any()),
        ("clear", 0) => ("kt_map_clear", vec![any()], Ty::Unit),
        _ => return None,
    })
}

/// The runtime function answering one member of a set.
///
/// A set's `size` and `isEmpty` are the map's: a set IS a map with no values, and both questions
/// are about its keys.
fn set_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        ("getSize" | "size", 0) => ("kt_map_size", vec![any()], Ty::Int),
        ("isEmpty", 0) => ("kt_map_is_empty", vec![any()], Ty::Boolean),
        ("contains", 1) => ("kt_set_contains", vec![any(), any()], Ty::Boolean),
        ("add", 1) => ("kt_set_add", vec![any(), any()], Ty::Boolean),
        ("remove", 1) => ("kt_set_remove", vec![any(), any()], Ty::Boolean),
        ("clear", 0) => ("kt_map_clear", vec![any()], Ty::Unit),
        _ => return None,
    })
}

/// The runtime function answering one member of a map entry.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses.
fn map_entry_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        ("key" | "getKey" | "component1", 0) => ("kt_map_entry_key", vec![any()], any()),
        ("value" | "getValue" | "component2", 0) => ("kt_map_entry_value", vec![any()], any()),
        _ => return None,
    })
}

/// The type one of those getters answers with, for the physical-type question.
pub(super) fn map_getter_ty(name: &str) -> Ty {
    match map_symbol(name, 0).or_else(|| set_symbol(name, 0)) {
        Some((_, _, answer)) => answer,
        None => any(),
    }
}

impl BodyLowering<'_, '_, '_> {
    /// `mapOf(…)` / `setOf(…)` and their relatives, or `None` when the declaration is something
    /// else.
    ///
    /// A vararg call arrives with its operands already packed into an `Array<T>`, and the runtime
    /// copies out of it: the array belongs to the caller, and a map can be written through.
    pub(super) fn map_construction(
        &mut self,
        owner: &str,
        name: &str,
        packs_a_vararg: bool,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_collections_facade(owner) {
            return None;
        }
        match (name, args) {
            // The empty forms. `mapOf()` with nothing in it is Kotlin's `emptyMap()`, and the
            // three named maps are one object here.
            ("emptyMap", []) | ("mapOf", []) | ("mutableMapOf", []) | ("hashMapOf", []) => {
                Some(self.runtime_call("kt_map_new", &[], any(), &[]))
            }
            ("emptySet", []) | ("setOf", []) | ("mutableSetOf", []) | ("hashSetOf", []) => {
                Some(self.runtime_call("kt_set_new", &[], any(), &[]))
            }
            ("mapOf" | "mutableMapOf" | "hashMapOf", [argument]) if packs_a_vararg => {
                Some(self.built_from("kt_map_of", *argument))
            }
            ("setOf" | "mutableSetOf" | "hashSetOf", [argument]) if packs_a_vararg => {
                Some(self.built_from("kt_set_of", *argument))
            }
            // The single-operand forms, which Kotlin declares beside the vararg ones. Only the
            // selected declaration's PHYSICAL parameter can say which this is, for the reason
            // `listOf` spells out: a vararg parameter is an array however its element type was
            // substituted, and `T` may itself be an array.
            ("mapOf", [pair]) => Some(self.built_from("kt_map_of_pair", *pair)),
            ("setOf", [element]) => Some(self.set_single(*element)),
            _ => None,
        }
    }

    /// One of those, over the array a vararg call packed.
    fn built_from(&mut self, symbol: &str, packed: u32) -> Result<Option<Value>, Unsupported> {
        let elements = self.reference(packed)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, &[any()], any(), &[elements])
    }

    /// `setOf(x)`: the one-element form, built by adding to an empty set rather than by packing an
    /// array first — there is nothing for an array to carry that the element does not.
    fn set_single(&mut self, element: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(element)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(set) = self.runtime_call("kt_set_new", &[], any(), &[])? else {
            return Ok(None);
        };
        self.runtime_call("kt_set_add", &[any(), any()], Ty::Boolean, &[set, value])?;
        Ok(Some(set))
    }

    /// A member of a runtime map, set or map entry, or `None` when the receiver is none of them.
    pub(super) fn map_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        // A file that declares its own map, set or entry puts an object of ITS own behind that
        // type, and the runtime would read a header that is not there. No static type tells the
        // two apart within a shape, so a receiver of a shape the file implements declines — the
        // same position `lists.rs` takes, and by the same test.
        if self.file.implements_collection_of(ty) {
            return None;
        }
        let (symbol, carried, answer) = if is_map(ty) {
            map_symbol(name, args.len())?
        } else if is_set(ty) {
            set_symbol(name, args.len())?
        } else if is_map_entry(ty) {
            map_entry_symbol(name, args.len())?
        } else {
            return None;
        };
        Some(self.map_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// A checked read of a map's or set's property the runtime answers — `m.size`, `m.keys` — by
    /// the RECEIVER, as every other one here is.
    pub(super) fn map_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        let ty = self.type_of(receiver)?;
        if self.file.implements_collection_of(ty) || !(is_map(ty) || is_set(ty) || is_map_entry(ty))
        {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        if is_map_entry(ty) {
            map_entry_symbol(&property.name, 0)?;
        } else if is_map(ty) {
            map_symbol(&property.name, 0)?;
        } else {
            set_symbol(&property.name, 0)?;
        }
        Some(property.name.clone())
    }

    fn map_call(
        &mut self,
        symbol: &str,
        carried: &[Ty],
        answer: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let mut operands = vec![self.reference(receiver)?];
        for (argument, ty) in args.iter().zip(&carried[1..]) {
            let Some(value) = self.coerce(*argument, *ty)? else {
                return Err(format!("a `Unit` operand of `{symbol}`"));
            };
            operands.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(value) = self.runtime_call(symbol, carried, answer, &operands)? else {
            return Ok(None);
        };
        self.convert(value, Some(answer), ret)
    }
}
