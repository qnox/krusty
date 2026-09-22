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
//! `Lazy` lives here as well — `by lazy { … }` is a delegate object like any other, and computing
//! its value is the one place the runtime calls back into emitted code.
//!
//! `Pair` lives here too, for the same reasons in miniature: `a to b` is a heap object of a
//! runtime-owned type holding two references, and the runtime answers its members so that the three
//! `kotlin.Any` declares agree with what Kotlin's data class says. `Triple` is not one of these.
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

/// Whether a type is the runtime's `Lazy`.
fn is_lazy(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|internal| internal.matches("kotlin/Lazy"))
}

/// The runtime function answering one member of a `Lazy`.
///
/// `getValue` is the delegate convention's own name for the same question, and it is an EXTENSION
/// of the lazy facade rather than a member — so it arrives with the lazy as its receiver and the
/// delegation's two operands as arguments, neither of which a lazy reads.
fn lazy_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        ("value", 0) | ("getValue", 2) => ("kt_lazy_value", vec![any()], any()),
        ("isInitialized", 0) => ("kt_lazy_is_initialized", vec![any()], Ty::Boolean),
        _ => return None,
    })
}

/// Whether a type is the one `Delegates.notNull()` and `Delegates.observable(…)` answer with.
///
/// `ReadWriteProperty<Any?, T>` is what both declarations return and what the delegate field is
/// typed by — no static type separates the two, which is why the RUNTIME dispatches on the object.
/// It is an INTERFACE a program may implement, so a file that does is declined before this is
/// reached, by the guard every runtime-answered dependency member sits behind.
fn is_read_write_property(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|internal| internal.matches("kotlin/properties/ReadWriteProperty"))
}

/// Whether a type is the runtime's `Pair`.
fn is_pair(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|internal| internal.matches("kotlin/Pair"))
}

/// The runtime function answering one member of a `Pair`.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses.
fn pair_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        ("first" | "component1", 0) => ("kt_pair_first", vec![any()], any()),
        ("second" | "component2", 0) => ("kt_pair_second", vec![any()], any()),
        _ => return None,
    })
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
        // `next`, and the narrow spellings a primitive iterator declares beside it. Each answers
        // the same object; what differs is only the type the CALL SITE expects, which the caller
        // converts to — the runtime boxes by the array's own element descriptor, so the unboxing
        // reads the bits that were written.
        (
            IterationRole::Iterator,
            "next" | "nextByte" | "nextShort" | "nextBoolean" | "nextFloat" | "nextDouble",
            0,
        ) => ("kt_iterator_next", vec![any()], any()),
        // The two walks a program hands a function value. Kotlin declares both on `Iterable`, and
        // a receiver typed by it may hold either of the two iterables this runtime has — so they
        // go through the same descriptor dispatch iteration itself does, and a range is walked as
        // readily as a list.
        (IterationRole::Iterable, "map", 1) => ("kt_iterable_map", vec![any(), any()], any()),
        (IterationRole::Iterable, "forEach", 1) => {
            ("kt_iterable_for_each", vec![any(), any()], Ty::Unit)
        }
        // `withIndex()` answers an ITERABLE, not a list: Kotlin's is lazy, and the loop consuming
        // it may stop early. The object it makes keeps the source until something asks it for an
        // iterator, and that iterator counts as it walks.
        (IterationRole::Iterable, "withIndex", 0) => ("kt_iterable_with_index", vec![any()], any()),
        // `xs.asSequence()`: the receiver, kept until something asks it for an iterator. Lazy, as
        // Kotlin's is, and it answers for any iterable this runtime has — including text, whose
        // sequence walks its characters.
        (IterationRole::Iterable, "asSequence", 0) => ("kt_sequence_of", vec![any()], any()),
        // `joinToString()` with every parameter left at its default, and no other form of it. The
        // stdlib declares six, all defaulted, and this backend has no `$default` synthetic of a
        // dependency to call — so the defaults would have to be written here, and the only one
        // worth writing is the whole-declaration one a program gets by passing nothing. A call
        // that passes anything keeps declining, with the argument it passed still in sight.
        (IterationRole::Iterable, "joinToString", 0) => (
            "kt_iterable_join_to_string",
            vec![any()],
            Ty::obj("kotlin/String"),
        ),
        _ => return None,
    })
}

/// Whether a type is TEXT, which wears the iterable role without being a collection.
///
/// `for (c in s)` walks a string, so text answers [`iteration_role_of`]; but `s.contains(t)`,
/// `s.reversed()` and `s.first()` are questions about TEXT — one text inside another, a text
/// reversed, its first unit — and the string entry points answer them further along the chain. A
/// collection's answers to those names are about its ELEMENTS, so handing a string to them would
/// compare a `Char` against a whole string. Hence [`walk_symbol`] is kept apart from
/// [`interface_symbol`], and text does not reach it.
fn is_text(ty: Ty) -> bool {
    ty.non_null().obj_internal().is_some_and(|internal| {
        [
            "kotlin/String",
            "kotlin/CharSequence",
            "kotlin/text/StringBuilder",
        ]
        .iter()
        .any(|candidate| internal.matches(candidate))
            || super::super::super::intrinsics::is_char_sequence(internal)
    })
}

/// Whether a type is the SEQUENCE the runtime makes.
///
/// A sequence is offered a NARROWER set of members than an iterable. Every walk this runtime has is
/// eager, and an eager `map` on a sequence is not Kotlin's: the transform would run for every
/// element where Kotlin runs it per element consumed, which a side effect sees and an endless
/// sequence never survives. So only the members whose answer is the same either way are answered —
/// its iterator, and the lazy `withIndex` — and the rest decline at the call site, where the type
/// the program named is still in sight.
fn is_sequence(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(super::super::super::intrinsics::is_sequence_type)
}

/// Whether a member may be asked of a SEQUENCE receiver; see [`is_sequence`].
fn lazy_over_a_sequence(name: &str) -> bool {
    matches!(name, "iterator" | "withIndex")
}

/// A member Kotlin declares over `Iterable` that the runtime answers by WALKING the receiver.
///
/// Apart from [`interface_symbol`] only because a TEXT receiver must not reach these; see
/// [`is_text`]. Everything else about them is the same — the receiver may be any of the iterables
/// this runtime has, and the dispatch is the descriptor's.
///
/// Each takes the function value the program wrote and hands it to the runtime as a reference. A
/// call that passes a DEFAULTED operand is not one of these: like `joinToString`, the arity matched
/// on is the written one, and the stdlib's other overloads keep declining with their arguments
/// still in sight.
fn walk_symbol(
    role: IterationRole,
    name: &str,
    arity: usize,
    ret: Ty,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    if role != IterationRole::Iterable {
        return None;
    }
    Some(match (name, arity) {
        // `xs.sortedWith(comparator)`: a new list, the receiver untouched. The comparator is an
        // ordinary function value of two arguments here, which is what a `Comparator` this runtime
        // makes IS — see `intrinsics::runtime_functional_interface`.
        ("sortedWith", 1) => ("kt_iterable_sorted_with", vec![any(), any()], any()),
        ("any", 1) => ("kt_iterable_any", vec![any(), any()], Ty::Boolean),
        ("any", 0) => ("kt_iterable_is_not_empty", vec![any()], Ty::Boolean),
        ("all", 1) => ("kt_iterable_all", vec![any(), any()], Ty::Boolean),
        ("none", 1) => ("kt_iterable_none", vec![any(), any()], Ty::Boolean),
        ("none", 0) => ("kt_iterable_is_empty", vec![any()], Ty::Boolean),
        ("count", 0) => ("kt_iterable_count", vec![any()], Ty::Int),
        ("count", 1) => ("kt_iterable_count_matching", vec![any(), any()], Ty::Int),
        ("filter", 1) => ("kt_iterable_filter", vec![any(), any()], any()),
        ("filterNot", 1) => ("kt_iterable_filter_not", vec![any(), any()], any()),
        ("first", 1) => ("kt_iterable_first_matching", vec![any(), any()], any()),
        ("firstOrNull", 1) => ("kt_iterable_first_or_null", vec![any(), any()], any()),
        ("last", 1) => ("kt_iterable_last_matching", vec![any(), any()], any()),
        ("fold", 2) => ("kt_iterable_fold", vec![any(), any(), any()], any()),
        ("forEachIndexed", 1) => ("kt_iterable_for_each_indexed", vec![any(), any()], Ty::Unit),
        ("toList", 0) => ("kt_iterable_to_list", vec![any()], any()),
        ("reversed", 0) => ("kt_iterable_reversed", vec![any()], any()),
        ("indexOf", 1) => ("kt_iterable_index_of", vec![any(), any()], Ty::Int),
        ("contains", 1) => ("kt_iterable_contains", vec![any(), any()], Ty::Boolean),
        // `sumOf` is declared once per width the selector may answer, and the ANSWER's type is
        // the selector's — so the CALL's own type says which entry point sums it. Reading the
        // declaration instead would not serve both providers: a jar realizes a function type as
        // `Function1`, and the width the selector answers is no longer written there. Summing at
        // one width and narrowing afterwards is a different answer on overflow, so a width with no
        // entry point keeps declining rather than borrowing another's.
        ("sumOf", 1) => match ret.non_null() {
            Ty::Int => ("kt_iterable_sum_of_int", vec![any(), any()], Ty::Int),
            Ty::Long => ("kt_iterable_sum_of_long", vec![any(), any()], Ty::Long),
            Ty::Double => ("kt_iterable_sum_of_double", vec![any(), any()], Ty::Double),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether a type is the `IndexedValue` a `withIndex` walk yields.
fn is_indexed_value(ty: Ty) -> bool {
    ty.non_null()
        .obj_internal()
        .is_some_and(|internal| internal.matches("kotlin/collections/IndexedValue"))
}

/// The runtime function answering one member of an `IndexedValue`.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses. The index is an `Int` the generator must not box to ask about,
/// which is why each signature is spelled out.
fn indexed_value_symbol(name: &str, arity: usize) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (name, arity) {
        ("index" | "component1", 0) => ("kt_indexed_value_index", vec![any()], Ty::Int),
        ("value" | "component2", 0) => ("kt_indexed_value_value", vec![any()], any()),
        _ => return None,
    })
}

/// The type one of those reads answers with: the index is an `Int`, the value a reference.
pub(super) fn indexed_value_getter_ty(name: &str) -> Ty {
    match indexed_value_symbol(name, 0) {
        Some((_, _, answer)) => answer,
        None => any(),
    }
}

/// The runtime function answering one list member, with the types it is carried at.
///
/// `size` and an index are `Int`s the generator must not box to ask about, which is why each
/// signature is spelled out rather than taken as all-references.
/// `physical` is the member's REALIZED parameter list, which is what recovers `removeAt`.
///
/// Kotlin's `MutableList` declares `remove(element: E)` and `removeAt(index: Int)`. There is no
/// `remove(Int)` in Kotlin — the rename is deliberate, and it exists precisely so that removing
/// the element `2` cannot be confused with removing the element AT `2`.
///
/// Both still arrive here as `remove`, for the reason [`kotlin_owner`] exists: `removeAt` is
/// REALIZED as `java.util.List.remove(int)`, and a mapped builtin whose realization names a
/// different physical member hands over that physical name — the same way `CharSequence.get`
/// arrives as `charAt` and `Number.toByte` as `byteValue`.
///
/// Only the PHYSICAL parameter can tell the two apart, and the distinction is not academic:
/// `MutableList<Int>.remove(10)` substitutes `E` to `Int`, so the SEMANTIC parameter of the
/// ELEMENT overload is `Int` as well, and reading that one turns "remove the element 10" into
/// "remove at index 10" — an out-of-range index where Kotlin simply answers `false`. The
/// realizations differ where it counts: the index one takes an `Int` and the element one takes the
/// erased reference, whatever `E` was substituted to.
fn list_symbol(name: &str, arity: usize, physical: &[Ty]) -> Option<(&'static str, Vec<Ty>, Ty)> {
    // The OPERAND is the last physical parameter, never the first. An extension carries its
    // receiver as a physical argument, so `plusAssign` arrives as `(Collection, Any)` — reading
    // the first told it the RECEIVER was a collection, which is always true and meant `xs += 1`
    // walked the integer as if it were one. A member has only the operand, where first and last
    // are the same.
    let operand = physical.last().map(|ty| ty.non_null());
    let by_index = matches!(operand, Some(Ty::Int));
    // Whether the operand is a thing to WALK rather than a single element; see `plusAssign`.
    let walkable = operand.is_some_and(|ty| {
        ty.is_array()
            || ty.obj_internal().is_some_and(|internal| {
                super::super::super::intrinsics::is_list_type(internal)
                    || super::super::super::intrinsics::iteration_role(internal).is_some()
            })
    });
    Some(match (name, arity) {
        // `List.size` is a Kotlin property over a Java method, so the provider may present the
        // getter under either spelling; both name the same question.
        ("getSize" | "size", 0) => ("kt_list_size", vec![any()], Ty::Int),
        ("isEmpty", 0) => ("kt_list_is_empty", vec![any()], Ty::Boolean),
        ("get", 1) => ("kt_list_get", vec![any(), Ty::Int], any()),
        // `first()` and `last()` with no predicate. The one-argument forms take a LAMBDA and are
        // a different question — an inline declaration whose body decides which element — so they
        // are not these and fall through.
        ("first", 0) => ("kt_list_first", vec![any()], any()),
        ("last", 0) => ("kt_list_last", vec![any()], any()),
        // `list.sortWith(comparator)`, which writes through the receiver and answers nothing.
        ("sortWith", 1) => ("kt_list_sort_with", vec![any(), any()], Ty::Unit),
        ("indexOf", 1) => ("kt_list_index_of", vec![any(), any()], Ty::Int),
        ("lastIndexOf", 1) => ("kt_list_last_index_of", vec![any(), any()], Ty::Int),
        ("contains", 1) => ("kt_list_contains", vec![any(), any()], Ty::Boolean),
        // The mutating half. A receiver that is not a growable list never reaches these: Kotlin
        // declares them on `MutableList` only, so naming one is already the proof.
        ("add", 1) => ("kt_mutable_list_add", vec![any(), any()], Ty::Boolean),
        ("add", 2) => (
            "kt_mutable_list_add_at",
            vec![any(), Ty::Int, any()],
            Ty::Unit,
        ),
        ("set", 2) => ("kt_mutable_list_set", vec![any(), Ty::Int, any()], any()),
        ("removeAt", 1) => ("kt_mutable_list_remove_at", vec![any(), Ty::Int], any()),
        // `removeAt`, under the name its realization gave it; see the note on this function.
        ("remove", 1) if by_index => ("kt_mutable_list_remove_at", vec![any(), Ty::Int], any()),
        ("remove", 1) => ("kt_mutable_list_remove", vec![any(), any()], Ty::Boolean),
        ("clear", 0) => ("kt_mutable_list_clear", vec![any()], Ty::Unit),
        // `list += x`. Kotlin declares `plusAssign` once per shape of right-hand side: one takes
        // the ELEMENT, the others take something to walk. The physical parameter is what tells
        // them apart, as it does for `remove`/`removeAt` above — after substitution an element of
        // type `List<T>` and a collection of them read alike.
        // `xs + x` and `xs + ys`: a NEW list, never a change to the receiver. Which one a call
        // means is the PHYSICAL parameter's answer, exactly as it is for `plusAssign` below.
        ("plus", 1) if walkable => ("kt_iterable_plus_all", vec![any(), any()], any()),
        ("plus", 1) => ("kt_iterable_plus_element", vec![any(), any()], any()),
        ("plusAssign", 1) if walkable => ("kt_mutable_list_add_all", vec![any(), any()], Ty::Unit),
        ("plusAssign", 1) => ("kt_mutable_list_plus_assign", vec![any(), any()], Ty::Unit),
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
        packs_a_vararg: bool,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_collections_facade(owner) {
            return None;
        }
        match (name, args) {
            ("emptyList", []) | ("listOf", []) => {
                Some(self.runtime_call("kt_list_empty", &[], any(), &[]))
            }
            // A growable list, empty or holding what the vararg call already packed. `mutableListOf`
            // and `arrayListOf` are the same list under two names; neither may SHARE the vararg
            // array the way `listOf` does, because a write through the list would reach it.
            ("mutableListOf" | "arrayListOf", []) => {
                Some(self.runtime_call("kt_mutable_list_new", &[], any(), &[]))
            }
            ("mutableListOf" | "arrayListOf", [argument]) if packs_a_vararg => {
                Some(self.mutable_list_of(*argument))
            }
            // Kotlin declares `listOf` twice, and which one this is decides whether the argument
            // IS the list's elements or is one OF them. Only the selected declaration's PHYSICAL
            // parameter can say, because a vararg one is an array however its element type was
            // substituted. The semantic parameter cannot: the single-element overload's is `T`, and
            // `T` may itself be an array — `listOf(anArray)` takes that overload and answers a list
            // of one array. Nor can the argument's node shape: `arrayOf(1, 2, 3)` lowers to the
            // very vararg node a packed call would have, so the two arrive looking identical.
            ("listOf", [argument]) if packs_a_vararg => Some(self.list_of(*argument)),
            ("listOf", [element]) => Some(self.list_single(*element)),
            _ => None,
        }
    }

    /// `mutableListOf(a, b, c)`: a growable list holding a COPY of the vararg array's elements.
    ///
    /// Copied rather than shared, which is the one place this differs from `listOf`: the array a
    /// vararg call built belongs to the caller, and a list that could be written through would
    /// reach back into it.
    fn mutable_list_of(&mut self, elements: u32) -> Result<Option<Value>, Unsupported> {
        let elements = self.reference(elements)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_mutable_list_of", &[any()], any(), &[elements])
    }

    /// `lazy { … }`, or `None` when the declaration is something else.
    ///
    /// Only the one-argument form is realized. The overloads taking a thread-safety mode or a lock
    /// decline by arity: this target has no threads, and answering one of those as if it were the
    /// plain form would silently drop what the program asked for.
    pub(super) fn lazy_construction(
        &mut self,
        owner: &str,
        name: &str,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_lazy_facade(owner) || name != "lazy" {
            return None;
        }
        let [initializer] = args else { return None };
        Some(self.lazy_of(*initializer))
    }

    fn lazy_of(&mut self, initializer: u32) -> Result<Option<Value>, Unsupported> {
        let function = self.reference(initializer)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_lazy_of", &[any()], any(), &[function])
    }

    /// `getValue(thisRef, property)` / `setValue(thisRef, property, value)` on one of those delegates.
    ///
    /// `thisRef` is nothing either delegate reads, and the delegation already evaluated whatever it
    /// names, so it is dropped as a lazy's is. The PROPERTY is carried differently by each half. On
    /// a WRITE it travels through as the object it is, because an observable hands it to its
    /// callback and nothing reads it on the way. On a READ only its NAME can matter — it is the
    /// text of the error a `notNull` read-before-write raises — and the runtime cannot ask the
    /// object for that, so the name is a literal here, taken from the reference operand's own
    /// declaration. A `KProperty` this file did not build has no name to take, and the read
    /// declines rather than reporting a wrong one.
    fn read_write_property_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        match (name, args) {
            ("getValue", [_, property]) => {
                let Some(text) = self.property_reference_name(*property) else {
                    return Some(Err(
                        "a delegated property read through a `KProperty` this file did not build"
                            .to_string(),
                    ));
                };
                Some(self.delegate_read(receiver, &text, ret))
            }
            ("setValue", [_, property, value]) => {
                Some(self.delegate_write(receiver, *property, *value))
            }
            _ => None,
        }
    }

    /// `ReadOnlyProperty.getValue` on a delegate this FILE declares.
    ///
    /// Unlike `ReadWriteProperty`, whose `notNull()` and `observable(…)` the runtime builds itself,
    /// this runtime makes no `ReadOnlyProperty` at all — so every object that can stand behind the
    /// type is a class of this file, and the dispatch among them is exhaustive. The last arm is
    /// still emitted and still fails loudly: an object of neither class cannot arrive, and saying
    /// so where it would is cheaper than reasoning about it later.
    ///
    /// A call through the interface cannot go through a slot: an override of a DEPENDENCY member
    /// takes a slot of its own, so the interface names no number to dispatch on. Testing the
    /// receiver against each implementor is what that leaves, and it is the same dispatch a
    /// collection member of a type this file implements already uses.
    pub(super) fn read_only_property_member(
        &mut self,
        internal: crate::types::TypeName,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if name != "getValue" || args.len() != 2 {
            return None;
        }
        let declared = self.file.implementors_of(internal);
        let implementors: Vec<(ClassId, u32, Vec<Ty>, Ty)> = declared
            .iter()
            .filter_map(|&class| {
                self.member_slot(class, name, args.len())
                    .map(|(slot, params, supplied)| (class, slot, params, supplied))
            })
            .collect();
        // A class whose `getValue` this generator cannot place leaves the set short, and a
        // dispatch missing an arm would answer the wrong body for it.
        if implementors.is_empty() || implementors.len() != declared.len() {
            return None;
        }
        Some(self.read_only_property_dispatch(&implementors, receiver, args, ret))
    }

    fn read_only_property_dispatch(
        &mut self,
        implementors: &[(ClassId, u32, Vec<Ty>, Ty)],
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        // Each operand once, at whatever type it already has; every arm converts from there.
        let mut operands = Vec::with_capacity(args.len());
        for argument in args {
            let Some(value) = self.expression(*argument)? else {
                return Ok(None);
            };
            if self.terminated {
                return Ok(None);
            }
            operands.push((value, self.type_of(*argument)));
        }
        let produced =
            self.dispatch_by_implementor_with(implementors, object, &operands, ret, |body, _| {
                // Unreachable: the arms above cover every class that can stand behind the type.
                body.runtime_call("kt_abstract_method_called", &[], Ty::Unit, &[])?;
                Ok(Some(body.builder.ins().iconst(types::I64, 0)))
            })?;
        Ok(produced)
    }

    /// The name a `KProperty` operand of the delegate convention carries, or `None` when the
    /// operand is not a property reference this file built.
    ///
    /// An IMPLICIT COERCION is looked through: the convention's parameter is `KProperty<*>` and the
    /// reference is narrower, so common lowering wraps it — and the wrapper changes nothing about
    /// which declaration the reference names. So is a read of a STATIC: a top-level delegated
    /// property's `KProperty` is built once in the file's initializer and the call site reads it
    /// from there, so the reference is the static's INITIALIZER rather than the operand itself.
    fn property_reference_name(&self, property: u32) -> Option<String> {
        let property = self.through_coercions(property);
        if let IrExpr::GetStatic(index) = self.file.ir.expr(property) {
            let init = self.file.ir.statics.get(*index as usize)?.init;
            // One level only. A static's initializer is the reference itself where this applies,
            // and following a chain of statics would be following assignments rather than reading
            // a declaration.
            return self.named_property_reference(self.through_coercions(init));
        }
        self.named_property_reference(property)
    }

    /// The name of a property-reference node, or `None` for anything else.
    fn named_property_reference(&self, property: u32) -> Option<String> {
        match self.file.ir.expr(property) {
            IrExpr::LocalPropertyReference { name, .. } => Some(name.to_string()),
            IrExpr::Checked(crate::ir::IrCheckedOperation::PropertyReference {
                target, ..
            }) => {
                let target = match target {
                    crate::fir::FirPropertyReferenceTarget::Module(property)
                    | crate::fir::FirPropertyReferenceTarget::SpecializedModule {
                        property, ..
                    } => property,
                    _ => return None,
                };
                Some(self.file.ir.checked_properties.get(target)?.name.clone())
            }
            _ => None,
        }
    }

    /// An expression with every implicit coercion around it stripped.
    fn through_coercions(&self, mut id: u32) -> u32 {
        while let IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } = self.file.ir.expr(id)
        {
            id = *arg;
        }
        id
    }

    fn delegate_read(
        &mut self,
        receiver: u32,
        property: &str,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let name = self.string_literal(property.as_bytes())?;
        let Some(produced) = self.runtime_call(
            "kt_rw_property_get",
            &[any(), any()],
            any(),
            &[object, name],
        )?
        else {
            return Ok(None);
        };
        self.convert(produced, Some(any()), ret)
    }

    /// The write, which carries the PROPERTY itself: an observable hands it to its callback, and
    /// nothing in the runtime reads it, so whatever object the delegation built travels straight
    /// through.
    fn delegate_write(
        &mut self,
        receiver: u32,
        property: u32,
        value: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        let named = self.reference(property)?;
        if self.terminated {
            return Ok(None);
        }
        let written = self.reference(value)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            "kt_rw_property_set",
            &[any(), any(), any()],
            Ty::Unit,
            &[object, named, written],
        )
    }

    /// `l.value` — a checked read of a dependency property the runtime answers.
    pub(super) fn lazy_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        if !self.type_of(receiver).is_some_and(is_lazy) {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        lazy_symbol(&property.name, 0)?;
        Some(property.name.clone())
    }

    /// `a to b`, or `None` when the declaration is something else.
    ///
    /// It is an extension of the tuples file facade rather than a member of anything, so it arrives
    /// with its left operand as the receiver and its right as the one argument.
    pub(super) fn pair_construction(
        &mut self,
        owner: &str,
        name: &str,
        receiver: u32,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_tuples_facade(owner) || name != "to" {
            return None;
        }
        let [second] = args else { return None };
        Some(self.pair_of(receiver, *second))
    }

    pub(super) fn pair_of(
        &mut self,
        first: u32,
        second: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let first = self.reference(first)?;
        if self.terminated {
            return Ok(None);
        }
        let second = self.reference(second)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_pair_of", &[any(), any()], any(), &[first, second])
    }

    /// `p.first` — a checked read of a dependency property the runtime answers.
    ///
    /// The RECEIVER is part of the question, not just the getter's name: a range declares `first`
    /// too, and answering a range's bound out of a pair's header reads it as a pointer.
    pub(super) fn pair_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        if !self.type_of(receiver).is_some_and(is_pair) {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        pair_symbol(&property.name, 0)?;
        Some(property.name.clone())
    }

    fn list_of(&mut self, elements: u32) -> Result<Option<Value>, Unsupported> {
        let array = self.reference(elements)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_list_of", &[any()], any(), &[array])
    }

    /// `listOf(x)`: the one-element array the list needs, since this overload hands over no array.
    fn list_single(&mut self, element: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(element)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_list_single", &[any()], any(), &[value])
    }

    /// `values.size` — a checked read of a dependency property whose getter the runtime answers.
    ///
    /// The receiver settles which objects the getter may be asked of, exactly as it does for a
    /// call. Returns the getter's name, so the caller answers it as an explicit call would.
    /// `iv.index` / `iv.value` — a checked read of a dependency property the runtime answers.
    ///
    /// Returns the getter's name, so the caller can route it through [`Self::list_member`] exactly
    /// as an explicit call to it would be.
    pub(super) fn indexed_value_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        if !self.type_of(receiver).is_some_and(is_indexed_value) {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        indexed_value_symbol(&property.name, 0)?;
        Some(property.name.clone())
    }

    pub(super) fn list_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        if !self.type_of(receiver).is_some_and(is_list) {
            return None;
        }
        let property = self.file.provider.external_property(target)?;
        list_symbol(&property.name, 0, &[])?;
        Some(property.name.clone())
    }

    /// A member of a runtime list or of its iterator, or `None` when the receiver is neither.
    pub(super) fn list_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        self.list_member_declared(name, receiver, args, ret, &[])
    }

    /// The same, told the member's REALIZED parameter list — see [`list_symbol`] for what it
    /// decides and why the semantic one cannot.
    pub(super) fn list_member_declared(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
        physical: &[Ty],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        // Iteration first: a `List` is iterated through the same dispatch an `Iterable` is, so the
        // one member both spellings share is answered in one place.
        if is_lazy(ty) {
            let (symbol, carried, answer) = lazy_symbol(name, args.len())?;
            // `getValue(thisRef, property)` hands over two operands a lazy has no use for; the
            // runtime takes the receiver alone, so they are not evaluated here either — the
            // delegation already evaluated whatever they name.
            return Some(self.list_call(symbol, &carried, answer, receiver, &[], ret));
        }
        // `comparator.compare(a, b)`. A `Comparator` this runtime makes is a function value of two
        // arguments, so its one member is the invoke every function value answers; the runtime
        // entry point is that invoke with the boxed answer unwrapped, which keeps the `Int` the
        // site asked for out of a box it would only be unwrapped from again.
        if name == "compare"
            && args.len() == 2
            && ty
                .non_null()
                .obj_internal()
                .is_some_and(super::super::super::intrinsics::is_comparator)
        {
            return Some(self.list_call(
                "kt_comparator_compare",
                &[any(), any(), any()],
                Ty::Int,
                receiver,
                args,
                ret,
            ));
        }
        if is_read_write_property(ty) {
            return self.read_write_property_member(name, receiver, args, ret);
        }
        if is_pair(ty) {
            let (symbol, carried, answer) = pair_symbol(name, args.len())?;
            return Some(self.list_call(symbol, &carried, answer, receiver, args, ret));
        }
        if is_indexed_value(ty) {
            let (symbol, carried, answer) = indexed_value_symbol(name, args.len())?;
            return Some(self.list_call(symbol, &carried, answer, receiver, args, ret));
        }
        if let Some(realized) = self.walking_member(name, receiver, args, ret) {
            return Some(realized);
        }
        // A LIST's OWN members — indexed access, size, removal — are about the runtime's list
        // REPRESENTATION rather than about walking, and no thunk of a class's would reach one. A
        // file putting a class of its own behind a list declines them; see
        // [`FileLowering::implements_collection_of`].
        if !is_list(ty) || self.file.implements_collection_of(ty) {
            return None;
        }
        let written = self.written_arity(name, args);
        let args = &args[args.len() - written..];
        let (symbol, carried, answer) = list_symbol(name, written, physical)?;
        Some(self.list_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// A member the runtime answers by WALKING the receiver, and nothing else.
    ///
    /// The walk reaches an object's elements through `iterator`, `hasNext` and `next`, which the
    /// runtime can ask of an object of the PROGRAM too: a walkable class carries a thunk for each
    /// of its own in its descriptor (`objects::WalkMembers`). So this is the one group of members
    /// a receiver of a shape this file implements need not decline — and it is asked by MEMBER
    /// rather than by shape, because a shape says nothing about which member a call is:
    /// `CharSequence` is walkable and `value[0]` is not a walk.
    ///
    /// `Map` and `Map.Entry` declare none of the three, so a class of this file behind one of
    /// those is still beyond a walk's reach; see
    /// [`FileLowering::implements_unwalkable_collection_of`].
    pub(super) fn walking_member(
        &mut self,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        if self.file.implements_unwalkable_collection_of(ty) {
            return None;
        }
        // A member Kotlin gives a SPECIAL BRIDGE is not a walk's to answer where this file puts a
        // class of its own behind the receiver's type. `Collection<E>.contains(x as E)` answers
        // the OVERRIDE for an implementor of the program's — the bridge is what decides, by
        // whether the argument can be what the declaration accepts — and a walk would compare
        // elements instead, which is a different question with a different answer
        // (`codegen/box/bridges/strListContains.kt`). Where the file implements nothing of the
        // shape the receiver is the runtime's own and the walk IS the member.
        if self.file.implements_collection_of(ty)
            && ty.non_null().obj_internal().is_some_and(|internal| {
                super::super::super::intrinsics::has_special_bridge(internal, name)
            })
        {
            return None;
        }
        // A receiver typed by a runtime-known collection type, or by a CLASS OF THIS FILE whose
        // own members answer for one — the runtime walks either. The two are the same walk, and a
        // program that wrote `xs.withIndex()` over a collection of its own is entitled to the same
        // answer.
        let role = super::super::super::intrinsics::iteration_role_of(ty)
            .or_else(|| self.file.walkable_role(ty))?;
        // A SEQUENCE takes only the members that are lazy either way; see `is_sequence`.
        if is_sequence(ty) && !lazy_over_a_sequence(name) {
            return None;
        }
        let written = self.written_arity(name, args);
        let args = &args[args.len() - written..];
        let over_text = is_text(ty);
        let (symbol, carried, answer) = interface_symbol(role, name, written).or_else(|| {
            (!over_text)
                .then(|| walk_symbol(role, name, written, ret))
                .flatten()
        })?;
        Some(self.list_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// How many operands the call was WRITTEN with.
    ///
    /// `joinToString()` written with nothing in the parentheses reaches this backend with six
    /// operands or with none, depending only on which provider selected the declaration: a klib
    /// has no `$default` synthetic to call, so an omitted argument is materialized as the
    /// declaration's own default instead. The two are the same call.
    fn written_arity(&self, name: &str, args: &[u32]) -> usize {
        if name == "joinToString" && self.is_defaulted_join(args) {
            0
        } else {
            args.len()
        }
    }

    /// A ZERO-ARGUMENT collection member asked of a type this file implements ITSELF.
    ///
    /// The runtime answers such a member for the objects IT makes, and a class of this file's is
    /// not one of them — which is why the caller declines. But the file knows every class of its
    /// own that could stand behind that static type, so the choice can be made here: test the
    /// receiver against each, dispatch on that implementor's own slot when it matches, and fall
    /// through to the runtime entry point otherwise.
    ///
    /// Zero arguments on purpose. An argument would have to cross at the DECLARATION's carriers in
    /// one arm and at the runtime entry point's in the other, and reconciling those is more than
    /// the receiver test this is. `iterator`, `hasNext` and `next` take none, which is every member
    /// in this group.
    ///
    /// `None` when the member takes arguments, when the runtime has no entry point for it, or when
    /// any implementor does not supply it — a partial chain would fall through to the runtime for
    /// an object the runtime cannot answer for.
    pub(super) fn implemented_member(
        &mut self,
        internal: crate::types::TypeName,
        name: &str,
        params: &[Ty],
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        // A member Kotlin gives a SPECIAL BRIDGE is not this dispatch's to make: a call through the
        // wide type answers the member's default rather than reaching the override when the
        // argument cannot be what the declaration accepts, and there is no bridge here to put in
        // front of an implementor's arm. Every one of them takes an argument, so the nullary
        // members are untouched.
        if super::super::super::intrinsics::has_special_bridge(internal, name) {
            return None;
        }
        let ty = self.type_of(receiver)?;
        // Which runtime entry point would have answered this. Three tables reach a member of a
        // runtime-known type: the ITERATION one, keyed on the role a receiver plays; the SCALAR
        // one, keyed on the owner AND the parameter types the declaration states, because that is
        // what tells `s[i]` from a member of the same name over something else; and the map one.
        let owner = internal.render();
        let (symbol, carried, answer) = super::super::super::intrinsics::iteration_role_of(ty)
            .and_then(|role| interface_symbol(role, name, args.len()))
            .or_else(|| super::super::super::intrinsics::scalar_member(&owner, name, params))
            .or_else(|| super::maps::runtime_symbol(&owner, name, args.len()))
            // `Comparable.compareTo` is answered from the DESCRIPTOR rather than from a table
            // keyed on a shape, so it is named here rather than found: one member, one entry
            // point, both operands references and the answer an `Int`.
            .or_else(|| {
                super::super::super::intrinsics::is_comparable_compare_to(&owner, name, params)
                    .then(|| ("kt_compare_any", vec![any(), any()], Ty::Int))
            })?;
        // An operand list the two sides state differently in LENGTH is not something a conversion
        // reconciles; the runtime entry point leads with the receiver, so one more than the
        // arguments is what it takes.
        if carried.len() != args.len() + 1 {
            return None;
        }
        let declared = self.file.implementors_of(internal);
        let implementors: Vec<(ClassId, u32, Vec<Ty>, Ty)> = declared
            .iter()
            .filter_map(|&class| {
                self.member_slot(class, name, args.len())
                    .map(|(slot, params, supplied)| (class, slot, params, supplied))
            })
            .collect();
        if implementors.is_empty() || implementors.len() != declared.len() {
            return None;
        }
        Some(self.member_by_implementor(
            &implementors,
            symbol,
            &carried,
            answer,
            receiver,
            args,
            ret,
        ))
    }

    /// One of those, with the runtime entry point as the last arm.
    fn member_by_implementor(
        &mut self,
        implementors: &[(ClassId, u32, Vec<Ty>, Ty)],
        symbol: &'static str,
        carried: &[Ty],
        answer: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        // Each operand is evaluated ONCE here, at whatever type it already has; every arm converts
        // from that to the one it wants. Evaluating per arm would run a side effect twice.
        let mut operands = Vec::with_capacity(args.len());
        for argument in args {
            let Some(value) = self.expression(*argument)? else {
                return Ok(None);
            };
            if self.terminated {
                return Ok(None);
            }
            operands.push((value, self.type_of(*argument)));
        }
        let runtime_params: Vec<Ty> = carried.to_vec();
        let runtime_operands = operands.clone();
        let produced = self.dispatch_by_implementor_with(
            implementors,
            object,
            &operands,
            answer,
            move |body, object| {
                let mut values = Vec::with_capacity(runtime_operands.len() + 1);
                values.push(object);
                for ((value, source), target) in
                    runtime_operands.iter().zip(runtime_params.iter().skip(1))
                {
                    let Some(converted) = body.convert(*value, *source, *target)? else {
                        return Ok(None);
                    };
                    values.push(converted);
                }
                body.runtime_call(symbol, &runtime_params, answer, &values)
            },
        )?;
        let Some(produced) = produced else {
            return Ok(None);
        };
        self.convert(produced, Some(answer), ret)
    }

    /// Whether every operand of a `joinToString` is the constant the stdlib declares it to default
    /// to, so that the call is the one a program wrote as `joinToString()`.
    ///
    /// Spelled out value by value rather than counted, because what this decides is whether six
    /// operands may be DROPPED: a call that passes a separator of its own must keep declining,
    /// with the argument it passed still in sight, and only the exact default run is droppable.
    fn is_defaulted_join(&self, args: &[u32]) -> bool {
        let [separator, prefix, postfix, limit, truncated, transform] = args else {
            return false;
        };
        let string = |id: &u32, expected: &str| {
            matches!(
                self.file.ir.expr(*id),
                IrExpr::Const(IrConst::String(value)) if value.as_str() == Some(expected)
            )
        };
        string(separator, ", ")
            && string(prefix, "")
            && string(postfix, "")
            && matches!(self.file.ir.expr(*limit), IrExpr::Const(IrConst::Int(-1)))
            && string(truncated, "...")
            && matches!(self.file.ir.expr(*transform), IrExpr::Const(IrConst::Null))
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
