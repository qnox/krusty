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

/// Whether a type is one of the list shapes the runtime builds.
///
/// `Iterable` is deliberately NOT one of them — a range is one, and answering a range's `size` out
/// of a list's header reads a bound as a pointer.
fn is_list(file: &FileLowering<'_>, ty: Ty) -> bool {
    file.mapped_collection(ty)
        .is_some_and(|collection| collection.kind == crate::types::CollectionKind::List)
}

/// Whether a type is `Collection` itself, which a list, a set and a map's views all are. Its
/// members the runtime answers by the receiver's descriptor rather than by one shape's header.
fn is_collection(file: &FileLowering<'_>, ty: Ty) -> bool {
    file.mapped_collection(ty)
        .is_some_and(|collection| collection.kind == crate::types::CollectionKind::Collection)
}

/// Whether the receiver has the exact standard-library declaration identity this native
/// implementation realizes.
fn has_standard_identity(ty: Ty, predicate: fn(crate::types::TypeName) -> bool) -> bool {
    ty.non_null().obj_internal().is_some_and(predicate)
}

/// The runtime function answering one member of a `Lazy`.
///
/// `getValue` is the delegate convention's own name for the same question, and it is an EXTENSION
/// of the lazy facade rather than a member — so it arrives with the lazy as its receiver and the
/// delegation's two operands as arguments, neither of which a lazy reads.
fn lazy_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    Some(match (signature.name, signature.params, signature.ret) {
        ("value", [], ret)
            if super::super::super::intrinsics::classifier_matches(
                signature.owner.physical(),
                "kotlin/Lazy",
            ) && ret.is_reference() =>
        {
            ("kt_lazy_value", vec![any()], any())
        }
        ("getValue", [_, _], ret)
            if super::super::super::intrinsics::is_lazy_facade(signature.owner)
                && ret.is_reference() =>
        {
            ("kt_lazy_value", vec![any()], any())
        }
        ("isInitialized", [], Ty::Boolean)
            if super::super::super::intrinsics::classifier_matches(
                signature.owner.physical(),
                "kotlin/Lazy",
            ) =>
        {
            ("kt_lazy_is_initialized", vec![any()], Ty::Boolean)
        }
        _ => return None,
    })
}

/// The runtime function answering one member of a `Pair`.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses.
fn pair_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    if !super::super::super::intrinsics::classifier_matches(
        signature.owner.physical(),
        "kotlin/Pair",
    ) {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("first" | "component1", [], ret) if ret.is_reference() => {
            ("kt_pair_first", vec![any()], any())
        }
        ("second" | "component2", [], ret) if ret.is_reference() => {
            ("kt_pair_second", vec![any()], any())
        }
        _ => return None,
    })
}

/// The runtime function answering one member of an `IndexedValue`.
///
/// `component1`/`component2` are what a destructuring reads, and are the same two questions under
/// the names the convention uses. The index is an `Int` the generator must not box to ask about,
/// which is why each signature is spelled out.
fn indexed_value_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    if !super::super::super::intrinsics::classifier_matches(
        signature.owner.physical(),
        "kotlin/collections/IndexedValue",
    ) {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("index" | "component1", [], Ty::Int) => ("kt_indexed_value_index", vec![any()], Ty::Int),
        ("value" | "component2", [], ret) if ret.is_reference() => {
            ("kt_indexed_value_value", vec![any()], any())
        }
        _ => return None,
    })
}

/// The type one of those reads answers with: the index is an `Int`, the value a reference.
pub(super) fn indexed_value_getter_ty(name: &str) -> Ty {
    match name {
        "index" | "component1" => Ty::Int,
        _ => any(),
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
fn list_symbol(
    declaration: crate::types::CollectionKind,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
    physical: &[Ty],
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    if !matches!(
        declaration,
        crate::types::CollectionKind::Collection | crate::types::CollectionKind::List
    ) {
        return None;
    }
    // The operand is the last physical parameter. This matters for mapped realizations because
    // the semantic parameter may have been substituted to the same Kotlin type in both overloads,
    // while the physical parameter still distinguishes the JVM-compatible representation.
    let operand = physical.last().map(|ty| ty.non_null());
    let by_index = matches!(operand, Some(Ty::Int));
    Some(match (signature.name, signature.params, signature.ret) {
        // `List.size` is a Kotlin property over a Java method, so the provider may present the
        // getter under either spelling; both name the same question.
        ("getSize" | "size", [], Ty::Int) => ("kt_list_size", vec![any()], Ty::Int),
        ("isEmpty", [], Ty::Boolean) => ("kt_list_is_empty", vec![any()], Ty::Boolean),
        ("get", [Ty::Int], ret) if ret.is_reference() => {
            ("kt_list_get", vec![any(), Ty::Int], any())
        }
        ("indexOf", [element], Ty::Int) if element.is_reference() => {
            ("kt_list_index_of", vec![any(), any()], Ty::Int)
        }
        ("lastIndexOf", [element], Ty::Int) if element.is_reference() => {
            ("kt_list_last_index_of", vec![any(), any()], Ty::Int)
        }
        ("contains", [element], Ty::Boolean) if element.is_reference() => {
            ("kt_list_contains", vec![any(), any()], Ty::Boolean)
        }
        ("containsAll", [elements], Ty::Boolean) if elements.is_reference() => (
            "kt_collection_contains_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        // The mutating half. A receiver that is not a growable list never reaches these: Kotlin
        // declares them on `MutableList` only, so naming one is already the proof.
        ("add", [element], Ty::Boolean) if element.is_reference() => {
            ("kt_mutable_list_add", vec![any(), any()], Ty::Boolean)
        }
        ("add", [Ty::Int, element], Ty::Unit) if element.is_reference() => (
            "kt_mutable_list_add_at",
            vec![any(), Ty::Int, any()],
            Ty::Unit,
        ),
        ("set", [Ty::Int, element], ret) if element.is_reference() && ret.is_reference() => {
            ("kt_mutable_list_set", vec![any(), Ty::Int, any()], any())
        }
        ("removeAt", [Ty::Int], ret) if ret.is_reference() => {
            ("kt_mutable_list_remove_at", vec![any(), Ty::Int], any())
        }
        // `removeAt`, under the name its realization gave it; see the note on this function.
        ("remove", [Ty::Int], ret) if by_index && ret.is_reference() => {
            ("kt_mutable_list_remove_at", vec![any(), Ty::Int], any())
        }
        ("remove", [element], Ty::Boolean) if element.is_reference() => {
            ("kt_mutable_list_remove", vec![any(), any()], Ty::Boolean)
        }
        ("clear", [], Ty::Unit) => ("kt_mutable_list_clear", vec![any()], Ty::Unit),
        ("addAll", [elements], Ty::Boolean) if elements.is_reference() => (
            "kt_mutable_collection_add_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        _ => return None,
    })
}

/// The runtime function answering one member declared on `Collection` itself, for a receiver
/// typed by no narrower collection. Which collection stands behind it is the descriptor's to say,
/// so each entry point asks it rather than reading one shape's header.
fn collection_symbol(
    declaration: crate::types::CollectionKind,
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Vec<Ty>, Ty)> {
    if declaration != crate::types::CollectionKind::Collection {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("getSize" | "size", [], Ty::Int) => ("kt_collection_size", vec![any()], Ty::Int),
        ("isEmpty", [], Ty::Boolean) => ("kt_collection_is_empty", vec![any()], Ty::Boolean),
        ("contains", [element], Ty::Boolean) if element.is_reference() => {
            ("kt_iterable_contains", vec![any(), any()], Ty::Boolean)
        }
        ("containsAll", [elements], Ty::Boolean) if elements.is_reference() => (
            "kt_collection_contains_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        // The mutating half, declared on `MutableCollection`, which maps onto the same classifier.
        ("add", [element], Ty::Boolean) if element.is_reference() => {
            ("kt_mutable_collection_add", vec![any(), any()], Ty::Boolean)
        }
        ("remove", [element], Ty::Boolean) if element.is_reference() => (
            "kt_mutable_collection_remove",
            vec![any(), any()],
            Ty::Boolean,
        ),
        ("addAll", [elements], Ty::Boolean) if elements.is_reference() => (
            "kt_mutable_collection_add_all",
            vec![any(), any()],
            Ty::Boolean,
        ),
        ("clear", [], Ty::Unit) => ("kt_mutable_collection_clear", vec![any()], Ty::Unit),
        _ => return None,
    })
}

/// The runtime's own entry point for a member: the last arm of a dispatch by implementor, taken
/// when the receiver is none of this file's classes.
struct RuntimeEntry<'c> {
    symbol: &'static str,
    /// The entry point's parameter carriers, receiver excluded.
    carried: &'c [Ty],
    /// What the entry point hands back.
    answer: Ty,
}

impl BodyLowering<'_, '_, '_> {
    /// `listOf(…)` / `emptyList()`, or `None` when the declaration is something else.
    ///
    /// A vararg call arrives with its elements already packed into an `Array<T>`, so the list is
    /// that array with a header — there is nothing to copy and nothing to count here.
    pub(super) fn list_construction(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        packs_a_vararg: bool,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_collections_facade(signature.owner)
            || !signature.ret.is_reference()
        {
            return None;
        }
        match (signature.name, signature.params, args) {
            ("emptyList", [], []) => Some(self.runtime_call("kt_list_empty", &[], any(), &[])),
            ("listOf", [], []) => Some(self.runtime_call("kt_list_empty", &[], any(), &[])),
            ("listOf", [_], []) if packs_a_vararg => {
                Some(self.runtime_call("kt_list_empty", &[], any(), &[]))
            }
            // A growable list, empty or holding what the vararg call already packed. `mutableListOf`
            // and `arrayListOf` are the same list under two names; neither may SHARE the vararg
            // array the way `listOf` does, because a write through the list would reach it.
            ("mutableListOf" | "arrayListOf", [_], []) if packs_a_vararg => {
                Some(self.runtime_call("kt_mutable_list_new", &[], any(), &[]))
            }
            ("mutableListOf" | "arrayListOf", [], []) => {
                Some(self.runtime_call("kt_mutable_list_new", &[], any(), &[]))
            }
            ("mutableListOf" | "arrayListOf", [_], [argument]) if packs_a_vararg => {
                Some(self.mutable_list_of(*argument))
            }
            // Kotlin declares `listOf` twice, and which one this is decides whether the argument
            // IS the list's elements or is one OF them. Only the selected declaration's PHYSICAL
            // parameter can say, because a vararg one is an array however its element type was
            // substituted. The semantic parameter cannot: the single-element overload's is `T`, and
            // `T` may itself be an array — `listOf(anArray)` takes that overload and answers a list
            // of one array. Nor can the argument's node shape: `arrayOf(1, 2, 3)` lowers to the
            // very vararg node a packed call would have, so the two arrive looking identical.
            ("listOf", [_], [argument]) if packs_a_vararg => Some(self.list_of(*argument)),
            ("listOf", [element], [value]) if element.is_reference() => {
                Some(self.list_single(*value))
            }
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
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_lazy_facade(signature.owner)
            || signature.name != "lazy"
            || !matches!(signature.params, [Ty::Fun(_)])
            || !signature.ret.is_reference()
        {
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
    /// `thisRef` is nothing either delegate reads, but an explicit operator call may supply an
    /// arbitrary expression, so it is still evaluated in source order. The PROPERTY is carried
    /// differently by each half. On
    /// a WRITE it travels through as the object it is, because an observable hands it to its
    /// callback and nothing reads it on the way. On a READ only its NAME can matter — it is the
    /// text of the error a `notNull` read-before-write raises — and the runtime cannot ask the
    /// object for that, so the name is a literal here, taken from the reference operand's own
    /// declaration. A `KProperty` this file did not build has no name to take, and the read
    /// declines rather than reporting a wrong one.
    fn read_write_property_member(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::classifier_matches(
            signature.owner.physical(),
            "kotlin/properties/ReadWriteProperty",
        ) {
            return None;
        }
        match (signature.name, signature.params, signature.ret, args) {
            ("getValue", [_, _], declared, [this_ref, property]) if declared.is_reference() => {
                let Some(text) = self.property_reference_name(*property) else {
                    return Some(Err(
                        "a delegated property read through a `KProperty` this file did not build"
                            .to_string(),
                    ));
                };
                Some(self.delegate_read(receiver, *this_ref, *property, &text, ret))
            }
            ("setValue", [_, _, _], Ty::Unit, [this_ref, property, value]) => {
                Some(self.delegate_write(receiver, *this_ref, *property, *value))
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
        target: crate::fir::ExternalCallableId,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::classifier_matches(
            signature.owner.physical(),
            "kotlin/properties/ReadOnlyProperty",
        ) || signature.name != "getValue"
            || !matches!(signature.params, [_, _])
            || !signature.ret.is_reference()
            || args.len() != 2
        {
            return None;
        }
        let declared = self.file.implementors_of(internal);
        let implementors: Vec<(ClassId, u32, Vec<Ty>, Ty)> = declared
            .iter()
            .filter_map(|&class| {
                self.external_override_slot(class, target)
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
            let init = self.file.ir.statics.get(*index as usize)?.init?;
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
            IrExpr::LocalPropertyReference(reference) => Some(reference.name.to_string()),
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
        this_ref: u32,
        property_object: u32,
        property: &str,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.reference(this_ref)?;
        if self.terminated {
            return Ok(None);
        }
        self.reference(property_object)?;
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
        this_ref: u32,
        property: u32,
        value: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.reference(this_ref)?;
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
        if !self
            .type_of(receiver)
            .is_some_and(|ty| has_standard_identity(ty, super::classifier_shapes::is_lazy))
        {
            return None;
        }
        let property = self.file.callables.property(target)?;
        if property.name.as_ref() != "value" || !property.result.is_reference() {
            return None;
        }
        Some(property.name.to_string())
    }

    /// `a to b`, or `None` when the declaration is something else.
    ///
    /// It is an extension of the tuples file facade rather than a member of anything, so it arrives
    /// with its left operand as the receiver and its right as the one argument.
    pub(super) fn pair_construction(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if !super::super::super::intrinsics::is_tuples_facade(signature.owner)
            || signature.name != "to"
            || !signature.receiver.is_some_and(Ty::is_reference)
            || !matches!(signature.params, [value] if value.is_reference())
            || !signature.ret.is_reference()
        {
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
        if !self
            .type_of(receiver)
            .is_some_and(|ty| has_standard_identity(ty, super::classifier_shapes::is_pair))
        {
            return None;
        }
        let property = self.file.callables.property(target)?;
        if !matches!(property.name.as_ref(), "first" | "second") || !property.result.is_reference()
        {
            return None;
        }
        Some(property.name.to_string())
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
        if !self
            .type_of(receiver)
            .is_some_and(|ty| has_standard_identity(ty, super::classifier_shapes::is_indexed_value))
        {
            return None;
        }
        let property = self.file.callables.property(target)?;
        match (property.name.as_ref(), property.result) {
            ("index", Ty::Int) => {}
            ("value", ret) if ret.is_reference() => {}
            _ => return None,
        }
        Some(property.name.to_string())
    }

    pub(super) fn list_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<String> {
        if !self
            .type_of(receiver)
            .is_some_and(|ty| is_list(self.file, ty) || is_collection(self.file, ty))
        {
            return None;
        }
        let property = self.file.callables.property(target)?;
        if !matches!(property.name.as_ref(), "getSize" | "size") || property.result != Ty::Int {
            return None;
        }
        Some(property.name.to_string())
    }

    /// A checked dependency-property read over one of the runtime-owned representations in this
    /// module. The selected property identity is validated by the corresponding getter helper.
    pub(super) fn list_property_member(
        &mut self,
        name: &str,
        receiver: u32,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?;
        let (symbol, answer) = if has_standard_identity(ty, super::classifier_shapes::is_lazy) {
            ("kt_lazy_value", any())
        } else if has_standard_identity(ty, super::classifier_shapes::is_pair) {
            match name {
                "first" => ("kt_pair_first", any()),
                "second" => ("kt_pair_second", any()),
                _ => return None,
            }
        } else if has_standard_identity(ty, super::classifier_shapes::is_indexed_value) {
            match name {
                "index" => ("kt_indexed_value_index", Ty::Int),
                "value" => ("kt_indexed_value_value", any()),
                _ => return None,
            }
        } else if is_list(self.file, ty) && !self.file.implements_collection_of(ty) {
            match name {
                "getSize" | "size" => ("kt_list_size", Ty::Int),
                _ => return None,
            }
        } else if is_collection(self.file, ty) && !self.file.implements_collection_of(ty) {
            match name {
                "getSize" | "size" => ("kt_collection_size", Ty::Int),
                _ => return None,
            }
        } else {
            return None;
        };
        Some(self.list_call(symbol, &[any()], answer, receiver, &[], ret))
    }

    /// The same, told the member's REALIZED parameter list — see [`list_symbol`] for what it
    /// decides and why the semantic one cannot.
    pub(super) fn list_member_declared(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
        physical: &[Ty],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let name = signature.name;
        let ty = self.type_of(receiver)?;
        // Iteration first: a `List` is iterated through the same dispatch an `Iterable` is, so the
        // one member both spellings share is answered in one place.
        if has_standard_identity(ty, super::classifier_shapes::is_lazy) {
            let (symbol, carried, answer) = lazy_symbol(signature)?;
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
            && matches!(signature.params, [_, _])
            && signature.ret == Ty::Int
            && super::super::super::intrinsics::classifier_matches(
                signature.owner.physical(),
                "kotlin/Comparator",
            )
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
        if has_standard_identity(ty, super::classifier_shapes::is_read_write_property) {
            return self.read_write_property_member(signature, receiver, args, ret);
        }
        if has_standard_identity(ty, super::classifier_shapes::is_pair) {
            let (symbol, carried, answer) = pair_symbol(signature)?;
            return Some(self.list_call(symbol, &carried, answer, receiver, args, ret));
        }
        if has_standard_identity(ty, super::classifier_shapes::is_indexed_value) {
            let (symbol, carried, answer) = indexed_value_symbol(signature)?;
            return Some(self.list_call(symbol, &carried, answer, receiver, args, ret));
        }
        // A LIST's OWN members — indexed access, size, removal — are about the runtime's list
        // REPRESENTATION rather than about walking, and no thunk of a class's would reach one. A
        // file putting a class of its own behind a list declines them; see
        // [`FileLowering::implements_collection_of`].
        let list = is_list(self.file, ty);
        if !(list || is_collection(self.file, ty)) || self.file.implements_collection_of(ty) {
            return None;
        }
        let semantic_owner = signature.owner.semantic_classifier()?;
        let declaration =
            classifier_shapes::mapped_collection(self.file.classifiers, semantic_owner)?.kind;
        let (symbol, carried, answer) = if list {
            list_symbol(declaration, signature, physical)?
        } else {
            collection_symbol(declaration, signature)?
        };
        Some(self.list_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// An iteration-protocol member the runtime answers through the receiver descriptor.
    ///
    /// The walk reaches an object's elements through `iterator`, `hasNext` and `next`, which the
    /// runtime can ask of an object of the PROGRAM too: a walkable class carries a thunk for each
    /// of its own in its descriptor (`objects::WalkMembers`). Algorithms implemented by stdlib
    /// bodies never reach this backend path.
    ///
    /// `Map` and `Map.Entry` declare none of the three, so a class of this file behind one of
    /// those is still beyond a walk's reach; see
    /// [`FileLowering::implements_unwalkable_collection_of`].
    pub(super) fn iteration_member(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
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
                classifier_shapes::mapped_collection(self.file.classifiers, internal).is_some_and(
                    |collection| {
                        super::super::super::intrinsics::has_special_bridge(
                            collection,
                            signature.name,
                        )
                    },
                )
            })
        {
            return None;
        }
        // A receiver typed by a runtime-known collection type, or by a CLASS OF THIS FILE whose
        // own members answer for one — the runtime walks either. The two are the same walk, and a
        // program that wrote `xs.withIndex()` over a collection of its own is entitled to the same
        // answer.
        let shape = self
            .file
            .iteration_shape_of(ty)
            .or_else(|| self.file.walkable_shape(ty))?;
        let declaration_shape = signature
            .owner
            .semantic_classifier()
            .and_then(|owner| self.file.iteration_shape_of(Ty::obj_name(owner)))
            .or_else(|| {
                signature
                    .receiver
                    .and_then(|receiver| self.file.iteration_shape_of(receiver))
            });
        let result_shape = self.file.iteration_shape_of(signature.ret);
        let (symbol, carried, answer) = super::super::super::intrinsics::iteration_runtime_member(
            shape,
            declaration_shape,
            result_shape,
            signature,
        )?;
        if carried.len() != args.len() + 1 {
            return None;
        }
        Some(self.list_call(symbol, &carried, answer, receiver, args, ret))
    }

    /// A complete stdlib collection declaration whose body the active provider cannot publish.
    /// Signature matching lives at the Native intrinsic boundary; this method only validates the
    /// actual receiver representation and evaluates the already-selected operands.
    pub(super) fn collection_algorithm_member(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        use super::super::super::intrinsics::CollectionAlgorithm;

        let operation = super::super::super::intrinsics::collection_algorithm(signature)?;
        let receiver_ty = self.type_of(receiver)?;
        let shape = self
            .file
            .iteration_shape_of(receiver_ty)
            .or_else(|| self.file.walkable_shape(receiver_ty))?;
        if matches!(
            shape,
            super::super::super::intrinsics::CollectionShape::MapEntry
        ) {
            return None;
        }
        let (symbol, carried, answer, operands): (&str, Vec<Ty>, Ty, &[u32]) = match operation {
            CollectionAlgorithm::Map if args.len() == 1 => {
                ("kt_iterable_map", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::ForEach if args.len() == 1 => {
                ("kt_iterable_for_each", vec![any(), any()], Ty::Unit, args)
            }
            CollectionAlgorithm::WithIndex if args.is_empty() => {
                ("kt_iterable_with_index", vec![any()], any(), args)
            }
            CollectionAlgorithm::JoinToStringDefaults
                if args.is_empty() || self.is_defaulted_join(args) =>
            {
                ("kt_iterable_join_to_string", vec![any()], Ty::String, &[])
            }
            CollectionAlgorithm::Any if args.len() == 1 => {
                ("kt_iterable_any", vec![any(), any()], Ty::Boolean, args)
            }
            CollectionAlgorithm::All if args.len() == 1 => {
                ("kt_iterable_all", vec![any(), any()], Ty::Boolean, args)
            }
            CollectionAlgorithm::None if args.len() == 1 => {
                ("kt_iterable_none", vec![any(), any()], Ty::Boolean, args)
            }
            CollectionAlgorithm::IsNotEmpty if args.is_empty() => {
                ("kt_iterable_is_not_empty", vec![any()], Ty::Boolean, args)
            }
            CollectionAlgorithm::IsEmpty if args.is_empty() => {
                ("kt_iterable_is_empty", vec![any()], Ty::Boolean, args)
            }
            CollectionAlgorithm::Count if args.is_empty() => {
                ("kt_iterable_count", vec![any()], Ty::Int, args)
            }
            CollectionAlgorithm::CountMatching if args.len() == 1 => (
                "kt_iterable_count_matching",
                vec![any(), any()],
                Ty::Int,
                args,
            ),
            CollectionAlgorithm::Filter if args.len() == 1 => {
                ("kt_iterable_filter", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::FilterNot if args.len() == 1 => {
                ("kt_iterable_filter_not", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::First if args.is_empty() => {
                ("kt_iterable_first", vec![any()], any(), args)
            }
            CollectionAlgorithm::FirstMatching if args.len() == 1 => (
                "kt_iterable_first_matching",
                vec![any(), any()],
                any(),
                args,
            ),
            CollectionAlgorithm::FirstOrNull if args.len() == 1 => {
                ("kt_iterable_first_or_null", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::Last if args.is_empty() => {
                ("kt_iterable_last", vec![any()], any(), args)
            }
            CollectionAlgorithm::LastMatching if args.len() == 1 => {
                ("kt_iterable_last_matching", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::Fold if args.len() == 2 => {
                ("kt_iterable_fold", vec![any(), any(), any()], any(), args)
            }
            CollectionAlgorithm::ForEachIndexed if args.len() == 1 => (
                "kt_iterable_for_each_indexed",
                vec![any(), any()],
                Ty::Unit,
                args,
            ),
            CollectionAlgorithm::ToList if args.is_empty() => {
                ("kt_iterable_to_list", vec![any()], any(), args)
            }
            CollectionAlgorithm::Reversed if args.is_empty() => {
                ("kt_iterable_reversed", vec![any()], any(), args)
            }
            CollectionAlgorithm::IndexOf if args.len() == 1 => {
                ("kt_iterable_index_of", vec![any(), any()], Ty::Int, args)
            }
            CollectionAlgorithm::Contains if args.len() == 1 => (
                "kt_iterable_contains",
                vec![any(), any()],
                Ty::Boolean,
                args,
            ),
            CollectionAlgorithm::SumOfInt if args.len() == 1 => {
                ("kt_iterable_sum_of_int", vec![any(), any()], Ty::Int, args)
            }
            CollectionAlgorithm::SumOfLong if args.len() == 1 => (
                "kt_iterable_sum_of_long",
                vec![any(), any()],
                Ty::Long,
                args,
            ),
            CollectionAlgorithm::SumOfDouble if args.len() == 1 => (
                "kt_iterable_sum_of_double",
                vec![any(), any()],
                Ty::Double,
                args,
            ),
            CollectionAlgorithm::PlusElement if args.len() == 1 => {
                ("kt_iterable_plus_element", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::PlusAll if args.len() == 1 => {
                ("kt_iterable_plus_all", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::AsSequence if args.is_empty() => {
                ("kt_sequence_of", vec![any()], any(), args)
            }
            CollectionAlgorithm::ToTypedArray if args.is_empty() => {
                ("kt_iterable_to_typed_array", vec![any()], any(), args)
            }
            CollectionAlgorithm::SortedWith if args.len() == 1 => {
                ("kt_iterable_sorted_with", vec![any(), any()], any(), args)
            }
            CollectionAlgorithm::SortWith
                if args.len() == 1
                    && is_list(self.file, receiver_ty)
                    && !self.file.implements_collection_of(receiver_ty) =>
            {
                ("kt_list_sort_with", vec![any(), any()], Ty::Unit, args)
            }
            CollectionAlgorithm::PlusAssignElement
                if args.len() == 1
                    && is_list(self.file, receiver_ty)
                    && !self.file.implements_collection_of(receiver_ty) =>
            {
                (
                    "kt_mutable_list_plus_assign",
                    vec![any(), any()],
                    Ty::Unit,
                    args,
                )
            }
            CollectionAlgorithm::PlusAssignAll
                if args.len() == 1
                    && is_list(self.file, receiver_ty)
                    && !self.file.implements_collection_of(receiver_ty) =>
            {
                (
                    "kt_mutable_list_add_all",
                    vec![any(), any()],
                    Ty::Unit,
                    args,
                )
            }
            _ => return None,
        };
        Some(self.list_call(symbol, &carried, answer, receiver, operands, ret))
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
        target: crate::fir::ExternalCallableId,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let name = signature.name;
        let params = signature.params;
        // A member Kotlin gives a SPECIAL BRIDGE is not this dispatch's to make: a call through the
        // wide type answers the member's default rather than reaching the override when the
        // argument cannot be what the declaration accepts, and there is no bridge here to put in
        // front of an implementor's arm. Every one of them takes an argument, so the nullary
        // members are untouched.
        if classifier_shapes::mapped_collection(self.file.classifiers, internal).is_some_and(
            |collection| super::super::super::intrinsics::has_special_bridge(collection, name),
        ) {
            return None;
        }
        let ty = self.type_of(receiver)?;
        // Which runtime entry point would have answered this. Three tables reach a member of a
        // runtime-known type: the ITERATION one, keyed on the receiver representation and complete
        // declaration signature; the SCALAR one, keyed on the owner AND the parameter types, because that is
        // what tells `s[i]` from a member of the same name over something else; and the map one.
        let owner = super::super::super::intrinsics::DeclarationOwner::callable(internal, None);
        let declaration_shape = signature
            .owner
            .semantic_classifier()
            .and_then(|owner| self.file.iteration_shape_of(Ty::obj_name(owner)))
            .or_else(|| {
                signature
                    .receiver
                    .and_then(|receiver| self.file.iteration_shape_of(receiver))
            });
        let result_shape = self.file.iteration_shape_of(signature.ret);
        let (symbol, carried, answer) = self
            .file
            .iteration_shape_of(ty)
            .and_then(|shape| {
                super::super::super::intrinsics::iteration_runtime_member(
                    shape,
                    declaration_shape,
                    result_shape,
                    signature,
                )
            })
            .or_else(|| super::super::super::intrinsics::scalar_member(owner, name, params))
            .or_else(|| super::maps::runtime_symbol(self.file, internal, signature))
            // `Comparable.compareTo` is answered from the DESCRIPTOR rather than from a table
            // keyed on a shape, so it is named here rather than found: one member, one entry
            // point, both operands references and the answer an `Int`.
            .or_else(|| {
                super::super::super::intrinsics::is_comparable_compare_to(signature)
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
                self.external_override_slot(class, target)
                    .map(|(slot, params, supplied)| (class, slot, params, supplied))
            })
            .collect();
        if implementors.is_empty() || implementors.len() != declared.len() {
            return None;
        }
        let runtime = RuntimeEntry {
            symbol,
            carried: &carried,
            answer,
        };
        Some(self.member_by_implementor(&implementors, runtime, receiver, args, ret))
    }

    /// One of those, with the runtime entry point as the last arm.
    fn member_by_implementor(
        &mut self,
        implementors: &[(ClassId, u32, Vec<Ty>, Ty)],
        runtime: RuntimeEntry<'_>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let RuntimeEntry {
            symbol,
            carried,
            answer,
        } = runtime;
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

    /// Whether every operand of `joinToString` is the declaration's own default value.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn signature<'a>(
        owner: &str,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> crate::native::intrinsics::FunctionSignature<'a> {
        crate::native::intrinsics::FunctionSignature::new(
            crate::native::intrinsics::DeclarationOwner::callable(
                crate::types::type_name(owner),
                None,
            ),
            name,
            params,
            ret,
        )
    }

    #[test]
    fn list_runtime_members_require_complete_signatures() {
        let element = Ty::ty_param("E", any());
        assert!(list_symbol(
            crate::types::CollectionKind::List,
            signature("kotlin/collections/List", "get", &[Ty::Int], element),
            &[Ty::Int],
        )
        .is_some());
        assert!(list_symbol(
            crate::types::CollectionKind::List,
            signature("kotlin/collections/List", "get", &[Ty::String], element),
            &[Ty::String],
        )
        .is_none());
        assert!(list_symbol(
            crate::types::CollectionKind::List,
            signature(
                "kotlin/collections/List",
                "contains",
                &[Ty::Int],
                Ty::Boolean
            ),
            &[any()],
        )
        .is_none());
        assert!(list_symbol(
            crate::types::CollectionKind::Set,
            signature("kotlin/collections/List", "get", &[Ty::Int], element),
            &[Ty::Int],
        )
        .is_none());
    }

    #[test]
    fn pair_runtime_members_reject_same_name_wrong_signatures() {
        let element = Ty::ty_param("A", any());
        assert!(pair_symbol(signature("kotlin/Pair", "component1", &[], element)).is_some());
        assert!(pair_symbol(signature("sample/Pair", "component1", &[], element,)).is_none());
        assert!(
            pair_symbol(signature("kotlin/Pair", "component1", &[Ty::Int], element,)).is_none()
        );
    }
}
