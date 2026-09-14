//! `listOf(...)` and what a program does with the list it answers.
//!
//! A list is an ordinary heap object of a runtime-owned type holding the `Array<T>` a vararg call
//! already built — which is what Kotlin's own `listOf(vararg)` wraps too, and what makes its
//! elements traced by a collector that already knows how to trace an array. The runtime answers its
//! members (`src/native/runtime/krusty_rt.c`, the `lists` section) so `equals`, `hashCode` and
//! `toString` agree with what the Kotlin declaration says.
//!
//! Only the READ-ONLY list is realized, and only `listOf`/`emptyList` produce one. `MutableList`,
//! `Set` and `Map` are not this type and decline by name, so a program using one is skipped rather
//! than answered wrongly.
//!
//! Which call reaches the runtime is decided by the RECEIVER's type, never by the owner declaring
//! the member: `iterator` is declared on `Iterable` and `hasNext` on `Iterator`, both of which a
//! user class may implement, and keying on the owner would send a user's object into the runtime.
//! Keying on the receiver is sound for the same reason it is for ranges — a file declaring a class
//! that implements one of these interfaces overrides a dependency method, and is declined whole.

use super::*;
use crate::native::intrinsics::IterationRole;

/// Whether a type is the read-only list the runtime builds.
///
/// `Collection` is its supertype and a `listOf` result reaches a member under that spelling too;
/// nothing else in this runtime is a `Collection`, so the two name the same objects. `Iterable` is
/// deliberately NOT one of them — a range is one, and answering a range's `size` out of a list's
/// header reads a bound as a pointer. A `MutableList` is absent for a plainer reason: nothing here
/// produces one, so a program holding one has no list this runtime can answer for.
fn is_list(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(super::super::super::intrinsics::is_list_type)
}

/// The runtime function answering one member through a receiver typed by an INTERFACE.
///
/// A generic body or an inlined stdlib extension — `for (e in this)` inside `Iterable<T>.forEach` —
/// types its receiver by the interface, and both of the things this runtime can iterate wear it. No
/// static type can say which, so these route to the runtime's own dispatch on the descriptor rather
/// than to either concrete answer. The concrete spellings are read first, by their own modules.
fn interface_symbol(
    role: IterationRole,
    name: &str,
    arity: usize,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (role, name, arity) {
        (IterationRole::Iterable, "iterator", 0) => ("kt_iterable_iterator", vec![any()], any()),
        (IterationRole::Iterator, "hasNext", 0) => {
            ("kt_iterator_has_next", vec![any()], Ty::Boolean)
        }
        (IterationRole::Iterator, "next", 0) => ("kt_iterator_next", vec![any()], any()),
        _ => return None,
    })
}

/// The runtime function answering one list member, with the types it is carried at.
///
/// `size` and an index are `Int`s the generator must not box to ask about, which is why each
/// signature is spelled out rather than taken as all-references.
fn list_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        // `List.size` is a Kotlin property over a Java method, so the provider may present the
        // getter under either spelling; both name the same question.
        ("getSize" | "size", 0) => ("kt_list_size", vec![any()], Ty::Int),
        ("isEmpty", 0) => ("kt_list_is_empty", vec![any()], Ty::Boolean),
        ("get", 1) => ("kt_list_get", vec![any(), Ty::Int], any()),
        ("indexOf", 1) => ("kt_list_index_of", vec![any(), any()], Ty::Int),
        ("lastIndexOf", 1) => ("kt_list_last_index_of", vec![any(), any()], Ty::Int),
        ("contains", 1) => ("kt_list_contains", vec![any(), any()], Ty::Boolean),
        _ => return None,
    })
}

impl BodyLowering<'_, '_, '_> {
    /// `listOf(…)` / `emptyList()`, or `None` when the declaration is something else.
    ///
    /// A vararg call arrives with its elements already packed into an `Array<T>`, so the list is
    /// that array with a header — there is nothing to copy and nothing to count here.
    pub(super) fn list_construction(
        &mut self,
        owner: &str,
        name: &str,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_collections_facade(owner) {
            return None;
        }
        match (name, args) {
            ("emptyList", []) | ("listOf", []) => {
                Some(self.runtime_call("kt_list_empty", &[], any(), &[]))
            }
            ("listOf", [elements]) => Some(self.list_of(*elements)),
            _ => None,
        }
    }

    fn list_of(&mut self, elements: u32) -> Result<Option<Value>, Unsupported> {
        // `listOf(single)` has its own overload taking the element itself rather than an array.
        // Which one this is, is a question about the ARGUMENT: an array reaches the list as it is,
        // and anything else is the one element of a new one.
        let Some(ty) = self.type_of(elements).map(Ty::non_null) else {
            return Err("`listOf` of an argument with no known type".to_string());
        };
        if !ty.is_reference_array() {
            return Err("`listOf` of a single element".to_string());
        }
        let array = self.reference(elements)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_list_of", &[any()], any(), &[array])
    }

    /// `values.size` — a checked read of a dependency property whose getter the runtime answers.
    ///
    /// The getter's NAME is all this needs; which objects it may be asked of is settled by the
    /// receiver, as it is for a call. Returns the getter's name, so the caller answers it exactly
    /// as an explicit call to it would be.
    pub(super) fn list_getter(&self, target: crate::fir::ExternalPropertyId) -> Option<String> {
        let property = self.file.classpath.external_property(target)?;
        let getter = self.file.classpath.external_callable(property.getter)?;
        list_symbol(&getter.callable.name, 0)?;
        Some(getter.callable.name.clone())
    }

    /// A member of a runtime list or of its iterator, or `None` when the receiver is neither.
    pub(super) fn list_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        // Iteration first: a `List` is iterated through the same dispatch an `Iterable` is, so the
        // one member both spellings share is answered in one place.
        let role = ty
            .non_null()
            .obj_internal()
            .and_then(super::super::super::intrinsics::iteration_role);
        let (symbol, carried, answer) =
            match role.and_then(|role| interface_symbol(role, name, args.len())) {
                Some(symbol) => symbol,
                None if is_list(ty) => list_symbol(name, args.len())?,
                None => return None,
            };
        Some(self.list_call(symbol, &carried, answer, receiver, args, ret))
    }

    fn list_call(
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
        if self.terminated {
            return Ok(Some(value));
        }
        self.convert(value, Some(answer), ret)
    }
}
