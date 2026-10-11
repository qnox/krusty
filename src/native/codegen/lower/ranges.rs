//! Ranges as VALUES: `val r = 1..3`, `x in r`, `r.first`.
//!
//! A range that a loop consumes never becomes an object — common lowering turns `for (i in 1..10)`
//! into a counted loop before this backend sees it, which is what the JVM's own range-loop
//! intrinsics do too. What reaches here is the other half: a range the program keeps, passes or
//! asks a question of. That one is an ordinary heap object of a runtime-owned type, and the runtime
//! answers its members (`src/native/runtime/krusty_rt.c`, the `ranges` section) so that `equals`,
//! `hashCode` and `toString` agree with the Kotlin declaration each type stands for.
//!
//! Three closed integral ranges are realized: `IntRange`, `LongRange` and `CharRange`, in the
//! `..` and `..<` forms. `downTo` never arrives as a construction — it is an ordinary extension
//! call — and the unsigned and floating-point ranges are declined by name, so a program using one
//! is skipped rather than answered wrongly.

use super::classifier_shapes::IntegralRangeElement;
use super::*;
use crate::fir::FirRangeOperation;
use crate::types::TypeName;

/// The element type of a runtime range, by the name of the type declaring the member.
///
/// `first` and `last` are declared on the PROGRESSION each range extends, not on the range, so a
/// read of one names that owner. Listing the progressions here is sound because this runtime builds
/// ONE object for a range and a progression alike — a pair of bounds with a step beside them — so
/// whichever of the two a program holds, its members answer at the same element width. The two
/// members a range gets from `ClosedRange` — `start` and `endInclusive` — are overridden by each
/// concrete range and so arrive under its own name; the interface is left out, because a user class
/// may implement it and would not be one of these objects.
fn integral_range_element_ty(element: IntegralRangeElement) -> Ty {
    match element {
        IntegralRangeElement::Int => Ty::Int,
        IntegralRangeElement::Long => Ty::Long,
        IntegralRangeElement::Char => Ty::Char,
        IntegralRangeElement::UInt => Ty::UInt,
        IntegralRangeElement::ULong => Ty::ULong,
    }
}

fn range_element(owner: TypeName) -> Option<Ty> {
    let element = super::classifier_shapes::integral_range_element(owner)?;
    Some(integral_range_element_ty(element))
}

/// The range a type names, as (runtime suffix, element). `Ty::Obj` rather than a name, because this
/// answers for the RESULT of a call that builds one.
fn range_type(ty: Ty) -> Option<(&'static str, Ty)> {
    let internal = ty.non_null().obj_internal()?;
    let element = super::classifier_shapes::integral_range_element(internal)?;
    let kind = match element {
        IntegralRangeElement::Int => "int",
        IntegralRangeElement::Long => "long",
        IntegralRangeElement::Char => "char",
        IntegralRangeElement::UInt => "uint",
        IntegralRangeElement::ULong => "ulong",
    };
    Some((kind, integral_range_element_ty(element)))
}

fn range_to_owner(owner: super::super::super::intrinsics::DeclarationOwner) -> bool {
    super::super::super::intrinsics::is_ranges_facade(owner)
        || owner.semantic_classifier().is_some_and(|owner| {
            [
                "kotlin/Int",
                "kotlin/Long",
                "kotlin/Char",
                "kotlin/UInt",
                "kotlin/ULong",
                "kotlin/Float",
                "kotlin/Double",
                "kotlin/Comparable",
            ]
            .into_iter()
            .any(|classifier| {
                super::super::super::intrinsics::classifier_matches(owner, classifier)
            })
        })
}

/// A FLOATING-POINT range, as (runtime suffix, element).
///
/// `0.0..2.0` answers a `ClosedFloatingPointRange<Double>`, and a variable may hold one under the
/// plainer `ClosedRange<Double>` too — so both names are read, and the TYPE ARGUMENT says which
/// width. Neither is a progression: there is no next floating-point number for Kotlin to name, so
/// there is no walk and no step, only a pair of bounds and the question `value in it`.
///
/// Both names are INTERFACES, which [`range_element`] deliberately leaves out because a user class
/// may implement one. It is safe here for the reason the member chain already relies on: a file
/// that declares such an implementation is declined at every member asked of a type it implements
/// itself, before this is reached.
fn floating_range(ty: Ty) -> Option<(&'static str, Ty)> {
    let Ty::Obj(internal, arguments) = ty.non_null() else {
        return None;
    };
    if !super::classifier_shapes::is_closed_floating_range(internal)
        && !super::classifier_shapes::is_closed_range(internal)
    {
        return None;
    }
    match arguments.first().map(|argument| argument.non_null()) {
        Some(Ty::Double) => Some(("double", Ty::Double)),
        Some(Ty::Float) => Some(("float", Ty::Float)),
        _ => None,
    }
}

/// The runtime function answering one member of a floating-point range, and what it answers with.
///
/// Both bounds are kept at `Double` whatever the range's width: widening a `Float` is exact and
/// order-preserving, so the comparison answers what float comparison would, and a `Float` range's
/// `start` narrows back to the very float it was built from.
fn floating_range_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Ty)> {
    let classifier = signature.owner.semantic_classifier();
    if !classifier.is_some_and(super::classifier_shapes::is_closed_floating_range)
        && !classifier.is_some_and(super::classifier_shapes::is_closed_range)
        && !super::super::super::intrinsics::is_ranges_facade(signature.owner)
    {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("contains", [_], Ty::Boolean) => ("kt_floating_range_contains", Ty::Boolean),
        ("isEmpty", [], Ty::Boolean) => ("kt_floating_range_is_empty", Ty::Boolean),
        _ => return None,
    })
}

/// An INTEGRAL range held under the interface `ClosedRange<T>`, as its element.
///
/// `fun f(range: ClosedRange<Int>) = x in range` names the interface rather than `IntRange`, and
/// `range_element` deliberately leaves the interface out because a user class may implement it. It
/// is safe to read here for the reason [`floating_range`] is: a file that declares such an
/// implementation is declined at every member asked of a type it implements itself, before this is
/// reached — and nothing else in this runtime wears the interface over a machine element.
fn closed_range_element(ty: Ty) -> Option<Ty> {
    let Ty::Obj(internal, arguments) = ty.non_null() else {
        return None;
    };
    if !super::classifier_shapes::is_closed_range(internal) {
        return None;
    }
    match arguments.first().map(|argument| argument.non_null()) {
        Some(element @ (Ty::Int | Ty::Long | Ty::Char | Ty::UInt | Ty::ULong)) => Some(element),
        _ => None,
    }
}

/// A COMPARABLE range: `"a".."c"`, and every other `a..b` whose bounds are ordered by `Comparable`
/// rather than by a machine comparison.
///
/// The TYPE ARGUMENT says which: a reference element is one no machine comparison reaches, so the
/// bounds are kept as objects and each comparison is the value's own. A type PARAMETER is not one
/// — `Ty::Obj` excludes it — because the object behind a `ClosedRange<T>` in a generic body may be
/// any of the three shapes, and only a concrete element rules the other two out.
///
/// The range carries the `compareTo` it orders by, chosen where it is built
/// ([`FunctionLowering::range_compare_function`]), so its members need nothing from the element's
/// descriptor and answer for any element the construction could order.
fn comparable_range_element(ty: Ty) -> Option<Ty> {
    let Ty::Obj(internal, arguments) = ty.non_null() else {
        return None;
    };
    if !super::classifier_shapes::is_closed_range(internal) {
        return None;
    }
    let argument = arguments.first()?.non_null();
    (matches!(argument, Ty::Obj(..)) && machine_carrier(argument) == Carrier::Ref)
        .then_some(argument)
}

/// The runtime function answering one member of a comparable range, and what it answers with.
fn comparable_range_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Ty)> {
    if !signature
        .owner
        .semantic_classifier()
        .is_some_and(super::classifier_shapes::is_closed_range)
        && !super::super::super::intrinsics::is_ranges_facade(signature.owner)
    {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("contains", [_], Ty::Boolean) => ("kt_comparable_range_contains", Ty::Boolean),
        ("isEmpty", [], Ty::Boolean) => ("kt_comparable_range_is_empty", Ty::Boolean),
        _ => return None,
    })
}

/// Whether this names text the runtime walks by UTF-16 unit.
///
/// Every `CharSequence` this target can produce IS a string — `subSequence` answers one and
/// nothing in the runtime makes another — which is the position `scalar_member` already takes for
/// `CharSequence.get`. So a `CharSequence` receiver reads its length the way a `String` does.
fn is_text(internal: TypeName) -> bool {
    internal == crate::types::wk::string() || super::classifier_shapes::is_char_sequence(internal)
}

/// The runtime function answering one range member, and the width it answers at. Every bound is
/// kept at 64 bits, so a member that answers one is a truncation of what the runtime returns.
fn range_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Ty)> {
    let classifier = signature.owner.semantic_classifier();
    if classifier.and_then(range_element).is_none()
        && !classifier.is_some_and(super::classifier_shapes::is_closed_range)
        && !super::super::super::intrinsics::is_ranges_facade(signature.owner)
    {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("contains", [_], Ty::Boolean) => ("kt_range_contains", Ty::Boolean),
        ("isEmpty", [], Ty::Boolean) => ("kt_range_is_empty", Ty::Boolean),
        // The iterator answers as a reference, so the narrowing below leaves it alone.
        ("iterator", [], ret) if ret.is_reference() => ("kt_range_iterator", any()),
        _ => return None,
    })
}

fn integral_range_property_symbol(name: &str) -> Option<(&'static str, Ty)> {
    Some(match name {
        "first" | "start" => ("kt_range_first", Ty::Long),
        "last" | "endInclusive" => ("kt_range_last", Ty::Long),
        "step" => ("kt_range_step_value", Ty::Long),
        _ => return None,
    })
}

fn floating_range_property_symbol(name: &str) -> Option<(&'static str, Ty)> {
    Some(match name {
        "start" => ("kt_floating_range_start", Ty::Double),
        "endInclusive" => ("kt_floating_range_end", Ty::Double),
        _ => return None,
    })
}

fn comparable_range_property_symbol(name: &str) -> Option<(&'static str, Ty)> {
    Some(match name {
        "start" => ("kt_comparable_range_start", any()),
        "endInclusive" => ("kt_comparable_range_end", any()),
        _ => return None,
    })
}

/// Whether a type is the iterator one of the runtime's ranges answers with.
///
/// The RECEIVER decides, never the call's owner: `hasNext` is declared on `Iterator`, which the
/// signature provider spells `java.util.Iterator` and which a user class may implement, so keying
/// on the owner would send a user iterator into the runtime. The three concrete iterators are safe
/// to key on because a file declaring its own subclass of one overrides a dependency method and is
/// declined whole.
fn is_range_iterator(ty: Ty) -> bool {
    let Some(internal) = ty.non_null().obj_internal() else {
        return false;
    };
    super::classifier_shapes::is_integral_range_iterator(internal)
}

/// The runtime function answering one member of a range's iterator.
fn range_iterator_symbol(
    signature: super::super::super::intrinsics::FunctionSignature<'_>,
) -> Option<(&'static str, Ty)> {
    let owner = signature.owner.semantic_classifier()?;
    if !super::classifier_shapes::is_integral_range_iterator(owner)
        && !super::super::super::intrinsics::classifier_matches(
            owner,
            "kotlin/collections/Iterator",
        )
    {
        return None;
    }
    Some(match (signature.name, signature.params, signature.ret) {
        ("hasNext", [], Ty::Boolean) => ("kt_range_iterator_has_next", Ty::Boolean),
        ("next", [], ret) if ret.is_reference() => ("kt_range_iterator_next", Ty::Long),
        ("nextInt", [], Ty::Int)
        | ("nextLong", [], Ty::Long)
        | ("nextChar", [], Ty::Char)
        | ("nextUInt", [], Ty::UInt)
        | ("nextULong", [], Ty::ULong) => ("kt_range_iterator_next", Ty::Long),
        _ => return None,
    })
}

impl BodyLowering<'_, '_, '_> {
    pub(super) fn range_construction(
        &mut self,
        operation: FirRangeOperation,
        start: u32,
        start_type: Ty,
        end: u32,
        end_type: Ty,
        result: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        // The checked RESULT is the selected declaration's semantic range type. It alone chooses
        // the representation: re-selecting one from operand types would hide a broken frontend
        // contract and can erase unsigned or custom-range semantics.
        let Some((kind, element)) = range_type(result) else {
            return Err(declined!(
                "a range of `{start_type:?}`..`{end_type:?}` as a value"
            ));
        };
        let suffix = match operation {
            FirRangeOperation::Through => "",
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => "_until",
            // An ordinary provider-selected extension call; it must not arrive as a construction.
            FirRangeOperation::DownTo => return Err("a `downTo` range as a value".into()),
        };
        let Some(first) = self.coerce(start, element)? else {
            return Ok(None);
        };
        let Some(last) = self.coerce(end, element)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            &format!("kt_{kind}_range{suffix}"),
            &[element, element],
            result,
            &[first, last],
        )
    }

    /// A member of a range object, or `a until b` on the facade that declares it.
    ///
    /// Returns `None` when this is neither, so the caller falls through to the ordinary
    /// dependency-member path. `contains` is the reason this is not simply a `runtime_member` entry:
    /// that path crosses every argument as a reference, which would box the very `Int` the question
    /// is about.
    pub(super) fn range_member(
        &mut self,
        signature: super::super::super::intrinsics::FunctionSignature<'_>,
        intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let owner = signature.owner;
        let name = signature.name;
        // `0.0..2.0` as a VALUE. A floating-point `rangeTo` reaches this backend as an ordinary
        // member call rather than as a range CONSTRUCTION, because there is no walk for common
        // lowering to turn into a counted loop — so the object is built here, and the type the
        // call RETURNS is what says which of the two widths it is.
        if name == "rangeTo"
            && matches!(signature.params, [_])
            && signature.ret.is_reference()
            && args.len() == 1
            && range_to_owner(signature.owner)
        {
            if let Some((kind, element)) = floating_range(ret) {
                return Some(self.floating_range_of(kind, element, receiver, args[0], ret));
            }
            // `"a".."c"`: the same shape one level up, with the bounds kept as OBJECTS. Kotlin
            // declares this `rangeTo` on `Comparable<T>`, so the receiver is whatever the program
            // is ordering and the type it RETURNS is what says the element is a reference.
            if let Some(element) = comparable_range_element(ret) {
                return Some(self.comparable_range_of(element, receiver, args[0], ret));
            }
        }
        // A member of one. The RECEIVER is what says so, never the owner: `contains` is declared
        // on the facade as well as on the interface, and `start` on the interface a user class may
        // implement.
        if floating_range(self.checked_type(receiver)?).is_some() {
            let (_, element) = floating_range(self.checked_type(receiver)?)?;
            let (symbol, answer) = floating_range_symbol(signature)?;
            return Some(self.floating_range_call(symbol, answer, element, receiver, args, ret));
        }
        // A member of a COMPARABLE range, keyed on the receiver for the same reason. The range
        // carries its own order, so any element the construction accepted can be asked about.
        if comparable_range_element(self.checked_type(receiver)?).is_some() {
            let (symbol, answer) = comparable_range_symbol(signature)?;
            return Some(self.comparable_range_call(symbol, answer, receiver, args, ret));
        }
        // The provider marks the exact selected `until` declaration from its complete signature;
        // the type it RETURNS says which range representation to build.
        if intrinsic == Some(crate::backend::BackendCompilerIntrinsic::RangeUntil)
            && matches!(signature.params, [_])
            && signature.ret.is_reference()
        {
            let [end] = args else {
                return Some(Err(
                    "an `until` call with an unexpected argument shape".into()
                ));
            };
            let (kind, element) = range_type(ret)?;
            return Some(self.range_of(
                &format!("kt_{kind}_range_until"),
                element,
                ret,
                receiver,
                *end,
            ));
        }
        // `downTo` is the descending counterpart. Its exact provider fact selects the operation;
        // the type it RETURNS says which range representation to build.
        if intrinsic == Some(crate::backend::BackendCompilerIntrinsic::RangeDownTo)
            && matches!(signature.params, [_])
            && signature.ret.is_reference()
        {
            let [end] = args else {
                return Some(Err(
                    "a `downTo` call with an unexpected argument shape".into()
                ));
            };
            let (kind, element) = range_type(ret)?;
            return Some(self.range_of(
                &format!("kt_{kind}_range_down_to"),
                element,
                ret,
                receiver,
                *end,
            ));
        }
        // `range step n` and `progression.reversed()`. Both answer a PROGRESSION, which this
        // runtime represents as the range it already had with a step beside it — so the receiver
        // carries its own width and there is nothing to pick from the result type. The step
        // crosses as a `Long` because every bound here is kept at 64 bits.
        if intrinsic == Some(crate::backend::BackendCompilerIntrinsic::ProgressionStep)
            && matches!(signature.params, [Ty::Int | Ty::Long])
            && signature.ret.is_reference()
        {
            if args.len() != 1 {
                return Some(Err(
                    "a progression `step` call with an unexpected argument shape".into(),
                ));
            }
            // The ANSWER is the progression, a reference; the ARGUMENT is the step, which crosses
            // at the runtime's 64 bits like every other bound. `Long` as the argument type serves
            // both widths: a step is never a `Char`, and an `Int` one widens into it.
            return Some(self.range_call("kt_range_step", any(), Ty::Long, receiver, args, ret));
        }
        if intrinsic == Some(crate::backend::BackendCompilerIntrinsic::ProgressionReversed)
            && signature.params.is_empty()
            && signature.ret.is_reference()
        {
            if !args.is_empty() {
                return Some(Err(
                    "a progression `reversed` call with an unexpected argument shape".into(),
                ));
            }
            return Some(self.range_call("kt_range_reversed", any(), any(), receiver, &[], ret));
        }
        if self.checked_type(receiver).is_some_and(is_range_iterator) {
            let (symbol, carried) = range_iterator_symbol(signature)?;
            // The element width the iterator answers at is the one the loop variable has.
            return Some(self.range_call(symbol, carried, ret.non_null(), receiver, args, ret));
        }
        // `x in range` where `x`'s type is not the range's element. Kotlin declares those as
        // extensions on the ranges FACADE — `RangesKt.contains(IntRange, Long)` — so the owner
        // names no range type and `range_element` below answers `None` for it. The range is the
        // receiver, so that is where the element comes from.
        //
        // The argument is carried at ITS OWN width and never coerced to the element, which is the
        // whole of the correctness. `4294967296L in 0..5` is `false`; truncating that `Long` to an
        // `Int` makes it 0 and answers `true`. The runtime keeps every bound at 64 bits and reads
        // them at the range's own signedness, so a value widened by its own signedness is already
        // the comparison Kotlin specifies.
        // Claimed only when the RECEIVER is a closed range. The facade declares `contains` over
        // collections and strings too, and answering `Some` for those took the call away from the
        // paths that already handle them: a list's `contains` started declining, which the
        // conformance lane did not see and `native_lists_e2e` did.
        if name == "contains"
            && matches!(signature.params, [_])
            && signature.ret == Ty::Boolean
            && args.len() == 1
            && super::super::super::intrinsics::is_ranges_facade(signature.owner)
            && range_element(owner.physical()).is_none()
            && self
                .checked_type(receiver)
                .map(Ty::non_null)
                .and_then(|ty| ty.obj_internal())
                .is_some_and(|internal| range_element(internal).is_some())
        {
            return Some(self.facade_range_contains(receiver, args[0]));
        }
        // A member of an integral range held under the INTERFACE, whose element the owner does not
        // name; see [`closed_range_element`]. The receiver answers it, and the bound it hands back
        // is boxed at the element's own width rather than left at the runtime's 64 bits, because
        // `ClosedRange`'s own declaration types it as the erased `T`.
        if let Some(element) = self.checked_type(receiver).and_then(closed_range_element) {
            let (symbol, carried) = range_symbol(signature)?;
            return Some(self.closed_range_call(symbol, carried, element, receiver, args, ret));
        }
        let element = range_element(owner.semantic_classifier()?)?;
        let (symbol, carried) = range_symbol(signature)?;
        Some(self.range_call(symbol, carried, element, receiver, args, ret))
    }

    /// A checked dependency-property read over a runtime range. Function calls use
    /// [`Self::range_member`] and therefore require a complete declaration signature; this path
    /// consumes the already-selected property identity from [`Self::range_getter`].
    pub(super) fn range_property_member(
        &mut self,
        owner: super::super::super::intrinsics::DeclarationOwner,
        name: &str,
        receiver: u32,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let receiver_ty = self.checked_type(receiver)?;
        if let Some((_, element)) = floating_range(receiver_ty) {
            let (symbol, answer) = floating_range_property_symbol(name)?;
            return Some(self.floating_range_call(symbol, answer, element, receiver, &[], ret));
        }
        if comparable_range_element(receiver_ty).is_some() {
            let (symbol, answer) = comparable_range_property_symbol(name)?;
            return Some(self.comparable_range_call(symbol, answer, receiver, &[], ret));
        }
        if let Some(element) = closed_range_element(receiver_ty) {
            let (symbol, carried) = integral_range_property_symbol(name)?;
            return Some(self.closed_range_call(symbol, carried, element, receiver, &[], ret));
        }
        let element = range_element(owner.physical())?;
        let (symbol, carried) = integral_range_property_symbol(name)?;
        Some(self.range_call(symbol, carried, element, receiver, &[], ret))
    }

    /// `a..b` on a receiver ordered by `Comparable`: the range object, its bounds kept as objects
    /// and the `compareTo` that orders them carried beside them.
    fn comparable_range_of(
        &mut self,
        element: Ty,
        start: u32,
        end: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let compare = self.range_compare_function(element)?;
        let first = self.reference(start)?;
        if self.terminated {
            return Ok(None);
        }
        let last = self.reference(end)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            "kt_comparable_range",
            &[any(), any(), any()],
            ret,
            &[first, last, compare],
        )
    }

    /// The address of the `compareTo` a range of `element` orders by: what the runtime's
    /// `kt_compare_fn` calls with two bounds, or a bound and a value.
    ///
    /// The element type decides, here, where it is known; the runtime is never left to rediscover
    /// the order from a bound's descriptor.
    ///
    /// - A final class of this file that declares `compareTo(other: Self): Int` passes that
    ///   function. It takes the receiver and the other object and answers an `Int`, which is the
    ///   runtime's `kt_compare_fn` exactly, and no subclass can override it.
    /// - A `String`, or any element in a file that declares no `Comparable` of its own, passes
    ///   `kt_compare_any`: every object such an element can hold is a builtin comparable, and the
    ///   builtin order is the runtime's.
    /// - Anything else declines. An open class could be overridden by the object behind the bound,
    ///   and an element that could hold both a builtin and a program object has no single function
    ///   to pass.
    fn range_compare_function(&mut self, element: Ty) -> Result<Value, Unsupported> {
        let Ty::Obj(name, _) = element.non_null() else {
            return Err(declined!(
                "a range of `{}`",
                super::type_checks::type_name_of(element)
            ));
        };
        let ir = self.file.ir;
        let Some(class) = ir.class_id_by_name(name) else {
            if name == crate::types::wk::string() || !self.file.declares_its_own_comparable {
                let id = self
                    .file
                    .import("kt_compare_any", &[any(), any()], Ty::Int)?;
                let func_ref = self.func_ref(id);
                return Ok(self.builder.ins().func_addr(types::I64, func_ref));
            }
            return Err(declined!(
                "a range of `{}` in a file that declares its own `Comparable`",
                name.render().replace('/', ".")
            ));
        };
        let declared = &ir.classes[class as usize];
        let is_final = !declared.is_open
            && !declared.is_abstract
            && !declared.is_interface
            && !declared.is_sealed
            && !self.file.values.is_value_class(declared.fq_name)
            && !declared.is_enum
            && !declared.is_enum_entry;
        let own = declared.methods.iter().copied().find(|&function| {
            let function = &ir.functions[function as usize];
            function.name == "compareTo"
                && function.dispatch_receiver.is_some()
                && function.ret == Ty::Int
                && matches!(function.params.as_slice(), [Ty::Obj(other, _)] if *other == name)
        });
        let (true, Some(function)) = (is_final, own) else {
            return Err(declined!(
                "a range of `{}`, which is not a final class declaring its own `compareTo`",
                name.render().replace('/', ".")
            ));
        };
        let Some(id) = self.file.functions[function as usize] else {
            return Err(declined!(
                "a range of `{}`, whose `compareTo` has no body",
                name.render().replace('/', ".")
            ));
        };
        let func_ref = self.func_ref(id);
        Ok(self.builder.ins().func_addr(types::I64, func_ref))
    }

    /// One member of a comparable range. Every operand crosses as a REFERENCE, which is what the
    /// bounds are: the comparison is the `compareTo` the range carries.
    fn comparable_range_call(
        &mut self,
        symbol: &str,
        answer: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        let mut params = vec![any()];
        let mut operands = vec![object];
        for argument in args {
            let value = self.reference(*argument)?;
            if self.terminated {
                return Ok(None);
            }
            params.push(any());
            operands.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(produced) = self.runtime_call(symbol, &params, answer, &operands)? else {
            return Ok(None);
        };
        self.convert(produced, Some(answer), ret)
    }

    /// One member of an integral range reached through `ClosedRange<T>`. The call is `range_call`'s
    /// — the runtime reads every bound at 64 bits — and only the ANSWER differs: the site's type is
    /// the erased `T`, so a bound goes back through the ELEMENT's width before it is boxed, which
    /// is what makes `(range as ClosedRange<Char>).start` read as the character it was built from.
    fn closed_range_call(
        &mut self,
        symbol: &str,
        carried: Ty,
        element: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if carried != Ty::Long {
            return self.range_call(symbol, carried, element, receiver, args, ret);
        }
        let Some(produced) = self.range_call(symbol, carried, element, receiver, args, element)?
        else {
            return Ok(None);
        };
        self.convert(produced, Some(element), ret)
    }

    /// `a..b` on a floating-point receiver: the range object, built at the width the result names.
    fn floating_range_of(
        &mut self,
        kind: &str,
        element: Ty,
        start: u32,
        end: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(first) = self.coerce(start, element)? else {
            return Ok(None);
        };
        let Some(last) = self.coerce(end, element)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            &format!("kt_{kind}_range"),
            &[element, element],
            ret,
            &[first, last],
        )
    }

    /// One member of a floating-point range. Every operand crosses at `Double`, which is exact for
    /// a `Float` and is the width the bounds are stored at.
    fn floating_range_call(
        &mut self,
        symbol: &str,
        answer: Ty,
        element: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        let mut params = vec![any()];
        let mut operands = vec![object];
        for argument in args {
            let Some(value) = self.coerce(*argument, Ty::Double)? else {
                return Ok(None);
            };
            params.push(Ty::Double);
            operands.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(produced) = self.runtime_call(symbol, &params, answer, &operands)? else {
            return Ok(None);
        };
        // A bound is answered at `Double` whatever the range's width, and the ELEMENT is what the
        // site expects. Going through it rather than straight to `ret` is what makes a `Float`
        // range's `start` right: `ClosedRange`'s own declaration types it as the erased `T`, so the
        // value is BOXED there, and a `Float` left in a `Double` box reads back as another number.
        let answered = if answer == Ty::Double {
            element
        } else {
            answer
        };
        let Some(narrowed) = self.convert(produced, Some(answer), answered)? else {
            return Ok(None);
        };
        self.convert(narrowed, Some(answered), ret)
    }

    /// `x in range` reached through the ranges facade, where `x` is not the range's element type.
    ///
    /// Declines a receiver that is not a range: the facade also declares `contains` over
    /// collections and strings, which this does not answer.
    fn facade_range_contains(
        &mut self,
        receiver: u32,
        argument: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(receiver_ty) = self.checked_type(receiver).map(Ty::non_null) else {
            return Err("`contains` on a receiver with no known type".into());
        };
        let Some(internal) = receiver_ty.obj_internal() else {
            return Err(declined!("`contains` on a `{receiver_ty:?}`"));
        };
        if range_element(internal).is_none() {
            return Err(declined!("`contains` on a `{receiver_ty:?}`"));
        }
        let Some(argument_ty) = self.checked_type(argument) else {
            return Err("`contains` of a value with no known type".into());
        };
        // Only a value that IS a scalar is answered. Kotlin's `contains` over a possibly-absent
        // element is a null test followed by the comparison — `null in 1..3` is false — and
        // reading a reference as a scalar without that test reads whatever the reference's bits
        // are: the corpus's `nullableInPrimitiveRange.kt` segfaulted on an `Int?` and answered
        // TRUE for the literal `null`.
        //
        // Stated as what is ADMITTED rather than as a list of what is not, because the list was
        // wrong: it named `Nullable` and `PlatformNullable`, and `null` written literally is
        // neither — it is `Ty::Null`, the type of the literal itself, which walked straight
        // through the guard.
        if !argument_ty.is_jvm_scalar() {
            return Err(declined!("`contains` of a `{argument_ty:?}`"));
        }
        let object = self.reference(receiver)?;
        let Some(value) = self.coerce(argument, argument_ty)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        // `Char` and the unsigned types zero-extend; a signed one extends its sign. Reading this
        // from the ARGUMENT rather than the range is what keeps an out-of-domain value outside.
        let signed = argument_ty != Ty::Char && !argument_ty.is_unsigned();
        let widened = self.resize(
            value,
            self.builder.func.dfg.value_type(value),
            signed,
            types::I64,
        );
        self.runtime_call(
            "kt_range_contains",
            &[any(), Ty::Long],
            Ty::Boolean,
            &[object, widened],
        )
    }

    /// `x in a..b` — a membership test the checker left as its BOUNDS rather than as a range.
    ///
    /// Nothing is constructed: the node carries the value and the two ends separately, so the whole
    /// of it is two comparisons. The bounds are still both EVALUATED, in source order after the
    /// value, because `a..b` builds a range before anything asks it a question and a program can
    /// see that — short-circuiting the second comparison would be an answer, but skipping the
    /// expression that produces it would be a missing effect.
    ///
    /// `downTo` descends, so its ends arrive the other way round: `x in a downTo b` is `b <= x <= a`.
    pub(super) fn range_contains(
        &mut self,
        operation: FirRangeOperation,
        value: u32,
        start: u32,
        end: u32,
        negated: bool,
        counter: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let counter = counter.non_null();
        if self.carrier(counter).clif().is_none() {
            return Err(declined!("a range membership test over `{counter:?}`"));
        }
        // Any of the three may TRANSFER CONTROL rather than answer — `x in 1u..break`, and the
        // same with `continue`, `return` and `throw`. The test is then unreachable and there is
        // nothing left to emit; only an operand that yielded no value while control stayed here is
        // a shape this cannot lower.
        let Some(value) = self.coerce(value, counter)? else {
            return if self.terminated {
                Ok(None)
            } else {
                Err("a `Unit` value in a range test".into())
            };
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(start) = self.coerce(start, counter)? else {
            return if self.terminated {
                Ok(None)
            } else {
                Err("a `Unit` range bound".into())
            };
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(end) = self.coerce(end, counter)? else {
            return if self.terminated {
                Ok(None)
            } else {
                Err("a `Unit` range bound".into())
            };
        };
        if self.terminated {
            return Ok(None);
        }
        // `Char` and the unsigned integers read their top bit as a value; every other counter is a
        // signed number, and `Char`'s own carrier is narrow enough that a signed compare would read
        // its upper half as negative.
        let signed = !counter.is_unsigned() && counter != Ty::Char;
        let (low, high, high_is_exclusive) = match operation {
            FirRangeOperation::Through => (start, end, false),
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => (start, end, true),
            // The ends are written in descending order, so the low one is the second.
            FirRangeOperation::DownTo => (end, start, false),
        };
        let above = self.builder.ins().icmp(
            if signed {
                IntCC::SignedLessThanOrEqual
            } else {
                IntCC::UnsignedLessThanOrEqual
            },
            low,
            value,
        );
        let below = self.builder.ins().icmp(
            match (signed, high_is_exclusive) {
                (true, false) => IntCC::SignedLessThanOrEqual,
                (true, true) => IntCC::SignedLessThan,
                (false, false) => IntCC::UnsignedLessThanOrEqual,
                (false, true) => IntCC::UnsignedLessThan,
            },
            value,
            high,
        );
        let inside = self.builder.ins().band(above, below);
        Ok(Some(match negated {
            false => inside,
            true => self.builder.ins().bxor_imm_u(inside, 1),
        }))
    }

    /// Whether a checked dependency property read names the `indices` extension.
    pub(super) fn external_getter_is_indices(
        &self,
        target: crate::fir::ExternalPropertyId,
    ) -> bool {
        let Some(property) = self.file.callables.property(target) else {
            return false;
        };
        let Some(getter) = self.file.callables.callable(property.getter) else {
            return false;
        };
        // The OWNER comes from the accessor, which is a type identity and not a spelling; the NAME
        // comes from the property, which is the only thing that knows what it is called.
        super::super::super::intrinsics::is_indices(
            super::super::super::intrinsics::DeclarationOwner::callable(
                getter.physical_owner,
                getter.declaration_owner,
            ),
            &property.name,
        )
    }

    /// `x.indices` — `0..size - 1` of an indexable receiver.
    ///
    /// Only a receiver whose size this generator can read qualifies: an array, or a `String`. The
    /// same extension property covers `Collection`, which needs a `size` there is no collection
    /// runtime to answer yet, so one of those declines by receiver rather than by name.
    pub(super) fn indices(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let Some(ty) = self.checked_type(receiver).map(Ty::non_null) else {
            return Err("`indices` of a receiver with no known type".into());
        };
        // A type parameter stands for whatever its bound admits, and `indices` is declared for the
        // bound: `fun <T : Collection<*>> f(c: T) = c.indices` asks the collection's question.
        let ty = match ty {
            Ty::TyParam(_, bound) => bound.non_null(),
            other => other,
        };
        let size = if ty.is_array() {
            self.array_size(receiver)?
        } else if ty == Ty::String || ty.obj_internal().is_some_and(is_text) {
            let value = self.reference(receiver)?;
            if self.terminated {
                return Ok(None);
            }
            self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])?
        } else if matches!(
            self.file.mapped_collection(ty),
            Some(crate::types::MappedCollection {
                kind: crate::types::CollectionKind::List,
                ..
            })
        ) && !self.file.implements_collection_of(ty)
        {
            // Every list the runtime hands out answers its size through one entry point, the same
            // one `size` itself reaches, so `indices` is that size read as a range.
            let value = self.reference(receiver)?;
            if self.terminated {
                return Ok(None);
            }
            self.runtime_call("kt_list_size", &[any()], Ty::Int, &[value])?
        } else if matches!(
            self.file.mapped_collection(ty),
            Some(crate::types::MappedCollection {
                kind: crate::types::CollectionKind::Collection
                    | crate::types::CollectionKind::List
                    | crate::types::CollectionKind::Set,
                ..
            })
        ) {
            // A collection that may be a SET has no list header to read. A collection implemented
            // by this program has no runtime list header either, but its descriptor publishes the
            // exact iterator/hasNext/next slots. In both cases its size is how many elements it
            // walks, so never reinterpret a program object as the runtime-owned `KList` layout.
            let value = self.reference(receiver)?;
            if self.terminated {
                return Ok(None);
            }
            self.runtime_call("kt_iterable_count", &[any()], Ty::Int, &[value])?
        } else {
            return Err(declined!("`indices` of a `{ty:?}`"));
        };
        let Some(size) = size else {
            return Ok(None);
        };
        let first = self.builder.ins().iconst(types::I32, 0);
        let last = self.builder.ins().iadd_imm_s(size, -1);
        self.runtime_call(
            "kt_int_range",
            &[Ty::Int, Ty::Int],
            Ty::obj("kotlin/ranges/IntRange"),
            &[first, last],
        )
    }

    /// The owner, name and result of a dependency getter naming a member of a range type.
    ///
    /// `range.first` reaches the generator as a checked read of a dependency property rather than as
    /// a call, so the getter behind it is resolved here and answered by the same runtime function
    /// the explicit call would reach.
    pub(super) fn range_getter(
        &self,
        target: crate::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Option<(super::super::super::intrinsics::DeclarationOwner, String)> {
        let property = self.file.callables.property(target)?;
        let getter = self.file.callables.callable(property.getter)?;
        // An integral range named by a CONCRETE range type is keyed on the owner. Every other
        // shape declares `start` and `endInclusive` on `ClosedRange`, an interface a user class
        // may implement, so there the RECEIVER is what says this is one of the runtime's — and
        // which of the three it is, since the interface is shared.
        let declaration_owner = getter.declaration_owner.and_then(|owner| match owner {
            crate::types::SemanticCallableOwner::Classifier(owner) => Some(owner),
            crate::types::SemanticCallableOwner::Package(_) => None,
        });
        if range_element(declaration_owner.unwrap_or(getter.physical_owner)).is_some() {
            integral_range_property_symbol(&property.name)?;
        } else {
            let ty = self.checked_type(receiver)?;
            if floating_range(ty).is_some() {
                floating_range_property_symbol(&property.name)?;
            } else if comparable_range_element(ty).is_some() {
                comparable_range_property_symbol(&property.name)?;
            } else if closed_range_element(ty).is_some() {
                integral_range_property_symbol(&property.name)?;
            } else {
                return None;
            }
        }
        Some((
            super::super::super::intrinsics::DeclarationOwner::callable(
                getter.physical_owner,
                getter.declaration_owner,
            ),
            property.name.to_string(),
        ))
    }

    /// `receiver until end`: both bounds at the element type, the range type as the result.
    fn range_of(
        &mut self,
        symbol: &str,
        element: Ty,
        result: Ty,
        receiver: u32,
        end: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let Some(first) = self.coerce(receiver, element)? else {
            return Ok(None);
        };
        let Some(last) = self.coerce(end, element)? else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, &[element, element], result, &[first, last])
    }

    fn range_call(
        &mut self,
        symbol: &str,
        carried: Ty,
        element: Ty,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let object = self.reference(receiver)?;
        let mut params = vec![any()];
        let mut arguments = vec![object];
        for argument in args {
            // `coerce` to the element type, then widen to the runtime's 64 bits with the
            // element's OWN signedness — which is what the runtime reads the bounds back with.
            // `Char` and the unsigned types zero-extend; getting that wrong makes `4294967295u`
            // arrive as `-1` and sit outside every range it belongs to.
            let Some(value) = self.coerce(*argument, element)? else {
                return Ok(None);
            };
            let signed = element != Ty::Char && !element.is_unsigned();
            let widened = self.resize(
                value,
                self.builder.func.dfg.value_type(value),
                signed,
                types::I64,
            );
            params.push(Ty::Long);
            arguments.push(widened);
        }
        if self.terminated {
            return Ok(None);
        }
        let Some(answer) = self.runtime_call(symbol, &params, carried, &arguments)? else {
            return Ok(None);
        };
        if carried != Ty::Long {
            return Ok(Some(answer));
        }
        // `IntRange.first`, `CharRange.first`, and `UIntRange.first` are narrower than the
        // runtime's answer. `ULong` is semantically distinct from `Long` but already has the same
        // machine width, so compare machine types before asking Cranelift to narrow.
        let Some(clif) = self.carrier(ret).clif() else {
            return Ok(Some(answer));
        };
        if self.builder.func.dfg.value_type(answer) == clif {
            return Ok(Some(answer));
        }
        Ok(Some(self.builder.ins().ireduce(clif, answer)))
    }

    /// A counted loop's `step` rejected a value that is not positive.
    ///
    /// The header already decided the step is illegal and stored it; this throws
    /// `IllegalArgumentException("Step must be positive, was: $step.")`, the same text the
    /// stdlib's `step` and `kt_range_step` use. The step is rendered through its own `toString`,
    /// so a `Long` step prints as a decimal without a type suffix.
    pub(super) fn illegal_progression_step(
        &mut self,
        step: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let step = self.reference(step)?;
        if self.terminated {
            return Ok(None);
        }
        let prefix = self.string_literal(b"Step must be positive, was: ")?;
        let Some(message) = self.runtime_call(
            "kt_string_plus",
            &[any(), any()],
            Ty::String,
            &[prefix, step],
        )?
        else {
            return Ok(None);
        };
        if self.terminated {
            return Ok(None);
        }
        let suffix = self.string_literal(b".")?;
        let Some(message) = self.runtime_call(
            "kt_string_plus",
            &[any(), any()],
            Ty::String,
            &[message, suffix],
        )?
        else {
            return Ok(None);
        };
        self.raise("kt_type_illegal_argument_exception", message)
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
                Some(crate::types::SemanticCallableOwner::Classifier(
                    crate::types::type_name(owner),
                )),
            ),
            name,
            params,
            ret,
        )
    }

    #[test]
    fn range_runtime_members_require_complete_signatures() {
        assert!(range_symbol(signature(
            "kotlin/ranges/IntRange",
            "contains",
            &[Ty::Int],
            Ty::Boolean,
        ))
        .is_some());
        assert!(range_symbol(signature(
            "sample/IntRange",
            "contains",
            &[Ty::Int],
            Ty::Boolean,
        ))
        .is_none());
        assert!(range_symbol(signature(
            "kotlin/ranges/IntRange",
            "contains",
            &[Ty::Int],
            Ty::Int,
        ))
        .is_none());
    }

    #[test]
    fn range_iterator_members_require_exact_results() {
        assert!(range_iterator_symbol(signature(
            "kotlin/collections/IntIterator",
            "nextInt",
            &[],
            Ty::Int,
        ))
        .is_some());
        assert!(range_iterator_symbol(signature(
            "kotlin/collections/IntIterator",
            "nextInt",
            &[],
            Ty::Long,
        ))
        .is_none());
    }
}
