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

use super::*;
use crate::fir::FirRangeOperation;
use crate::types::TypeName;

/// Which runtime range one pair of operand types selects, and the element they are carried as.
///
/// The selection is Kotlin's own overload set for `rangeTo`, and it is the same one
/// `JvmLibraries::range_construction` makes: the narrow integral types widen to `Int`, `Long` wins
/// over `Int`, and `Char` ranges only over `Char`.
fn element(start: Ty, end: Ty) -> Option<(&'static str, Ty)> {
    match (start, end) {
        (Ty::Char, Ty::Char) => Some(("char", Ty::Char)),
        (left, right) if left.is_int_range_operand() && right.is_int_range_operand() => {
            Some(("int", Ty::Int))
        }
        (left, right)
            if (left.is_int_range_operand() || left == Ty::Long)
                && (right.is_int_range_operand() || right == Ty::Long) =>
        {
            Some(("long", Ty::Long))
        }
        _ => None,
    }
}

/// The element type of a runtime range, by the name of the type declaring the member.
///
/// `first` and `last` are declared on the PROGRESSION each range extends, not on the range, so a
/// read of one names that owner. Listing the progressions here is sound because this runtime builds
/// ONE object for a range and a progression alike — a pair of bounds with a step beside them — so
/// whichever of the two a program holds, its members answer at the same element width. The two
/// members a range gets from `ClosedRange` — `start` and `endInclusive` — are overridden by each
/// concrete range and so arrive under its own name; the interface is left out, because a user class
/// may implement it and would not be one of these objects.
fn range_element(owner: TypeName) -> Option<Ty> {
    [
        ("kotlin/ranges/IntRange", Ty::Int),
        ("kotlin/ranges/IntProgression", Ty::Int),
        ("kotlin/ranges/LongRange", Ty::Long),
        ("kotlin/ranges/LongProgression", Ty::Long),
        ("kotlin/ranges/CharRange", Ty::Char),
        ("kotlin/ranges/CharProgression", Ty::Char),
        // The unsigned pair. Their members answer at the element's own width, and the runtime reads
        // the bounds unsigned, so a `UIntRange`'s `last` of `4294967295u` answers as itself.
        ("kotlin/ranges/UIntRange", Ty::UInt),
        ("kotlin/ranges/UIntProgression", Ty::UInt),
        ("kotlin/ranges/ULongRange", Ty::ULong),
        ("kotlin/ranges/ULongProgression", Ty::ULong),
    ]
    .into_iter()
    .find_map(|(candidate, element)| owner.matches(candidate).then_some(element))
}

/// The range a type names, as (runtime suffix, element). `Ty::Obj` rather than a name, because this
/// answers for the RESULT of a call that builds one.
fn range_type(ty: Ty) -> Option<(&'static str, Ty)> {
    let internal = ty.non_null().obj_internal()?;
    [
        ("kotlin/ranges/IntRange", "int", Ty::Int),
        ("kotlin/ranges/LongRange", "long", Ty::Long),
        ("kotlin/ranges/CharRange", "char", Ty::Char),
        // A PROGRESSION is what `downTo` answers — `10 downTo 1` is an `IntProgression`, not an
        // `IntRange` — and this runtime builds one object for both, a range carrying its step. So
        // the progression names map to the same constructors, and the step is what differs.
        ("kotlin/ranges/IntProgression", "int", Ty::Int),
        ("kotlin/ranges/LongProgression", "long", Ty::Long),
        ("kotlin/ranges/CharProgression", "char", Ty::Char),
        ("kotlin/ranges/UIntRange", "uint", Ty::UInt),
        ("kotlin/ranges/UIntProgression", "uint", Ty::UInt),
        ("kotlin/ranges/ULongRange", "ulong", Ty::ULong),
        ("kotlin/ranges/ULongProgression", "ulong", Ty::ULong),
    ]
    .into_iter()
    .find_map(|(candidate, kind, element)| internal.matches(candidate).then_some((kind, element)))
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
    let named = [
        "kotlin/ranges/ClosedFloatingPointRange",
        "kotlin/ranges/ClosedRange",
    ]
    .iter()
    .any(|candidate| internal.matches(candidate));
    if !named {
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
fn floating_range_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("contains", 1) => ("kt_floating_range_contains", Ty::Boolean),
        ("isEmpty", 0) => ("kt_floating_range_is_empty", Ty::Boolean),
        // The PROPERTY names, as in `range_symbol`: a property's own name is what its declaration
        // publishes, where the accessor's is a physical call target.
        ("start", 0) => ("kt_floating_range_start", Ty::Double),
        ("endInclusive", 0) => ("kt_floating_range_end", Ty::Double),
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
    if !internal.matches("kotlin/ranges/ClosedRange") {
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
/// Whether the runtime may ANSWER for such a range is a second question, asked by
/// [`FileLowering::declares_its_own_comparable`] at each site: the order is read from the value's
/// descriptor, so it is the runtime's own, and a class of the program's standing behind the
/// element has none there. That is the position `Comparable.compareTo` already takes.
fn comparable_range_element(ty: Ty) -> Option<Ty> {
    let Ty::Obj(internal, arguments) = ty.non_null() else {
        return None;
    };
    if !internal.matches("kotlin/ranges/ClosedRange") {
        return None;
    }
    let argument = arguments.first()?.non_null();
    (matches!(argument, Ty::Obj(..)) && carrier(argument) == Carrier::Ref).then_some(argument)
}

/// The runtime function answering one member of a comparable range, and what it answers with.
fn comparable_range_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("contains", 1) => ("kt_comparable_range_contains", Ty::Boolean),
        ("isEmpty", 0) => ("kt_comparable_range_is_empty", Ty::Boolean),
        ("start", 0) => ("kt_comparable_range_start", any()),
        ("endInclusive", 0) => ("kt_comparable_range_end", any()),
        _ => return None,
    })
}

/// Whether this names text the runtime walks by UTF-16 unit.
///
/// Every `CharSequence` this target can produce IS a string — `subSequence` answers one and
/// nothing in the runtime makes another — which is the position `scalar_member` already takes for
/// `CharSequence.get`. So a `CharSequence` receiver reads its length the way a `String` does.
fn is_text(internal: TypeName) -> bool {
    ["kotlin/String", "kotlin/CharSequence"]
        .iter()
        .any(|candidate| internal.matches(candidate))
}

/// The runtime function answering one range member, and the width it answers at. Every bound is
/// kept at 64 bits, so a member that answers one is a truncation of what the runtime returns.
fn range_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("contains", 1) => ("kt_range_contains", Ty::Boolean),
        ("isEmpty", 0) => ("kt_range_is_empty", Ty::Boolean),
        // The PROPERTY names, not the accessor spellings they are realized under. A property's own
        // name is what its declaration publishes; the accessor's is a physical call target, which
        // for an unsigned range is `getStart-pVg5ArA` and names nothing a table can be written in.
        ("first" | "start", 0) => ("kt_range_first", Ty::Long),
        ("last" | "endInclusive", 0) => ("kt_range_last", Ty::Long),
        // The iterator answers as a reference, so the narrowing below leaves it alone.
        ("iterator", 0) => ("kt_range_iterator", any()),
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
    [
        "kotlin/collections/IntIterator",
        "kotlin/collections/LongIterator",
        "kotlin/collections/CharIterator",
        "kotlin/collections/UIntIterator",
        "kotlin/collections/ULongIterator",
    ]
    .iter()
    .any(|candidate| internal.matches(candidate))
}

/// The runtime function answering one member of a range's iterator.
fn range_iterator_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("hasNext", 0) => ("kt_range_iterator_has_next", Ty::Boolean),
        ("next" | "nextInt" | "nextLong" | "nextChar" | "nextUInt" | "nextULong", 0) => {
            ("kt_range_iterator_next", Ty::Long)
        }
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
        // The RESULT says which range to build, which is the position `until` and `downTo` below
        // already take: `1u..5u` answers a `UIntRange`, whose bounds are read UNSIGNED, and the
        // two operands alone cannot say so — an unsigned pair reads as a plain integer one. The
        // operands remain the fallback, for a result typed more loosely than they are.
        let Some((kind, element)) =
            range_type(result).or_else(|| element(start_type.non_null(), end_type.non_null()))
        else {
            return Err(format!(
                "a range of `{start_type:?}`..`{end_type:?}` as a value"
            ));
        };
        let suffix = match operation {
            FirRangeOperation::Through => "",
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => "_until",
            // An ordinary provider-selected extension call; it must not arrive as a construction.
            FirRangeOperation::DownTo => return Err("a `downTo` range as a value".to_string()),
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
        owner: &str,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        // `0.0..2.0` as a VALUE. A floating-point `rangeTo` reaches this backend as an ordinary
        // member call rather than as a range CONSTRUCTION, because there is no walk for common
        // lowering to turn into a counted loop — so the object is built here, and the type the
        // call RETURNS is what says which of the two widths it is.
        if name == "rangeTo" && args.len() == 1 {
            if let Some((kind, element)) = floating_range(ret) {
                return Some(self.floating_range_of(kind, element, receiver, args[0], ret));
            }
            // `"a".."c"`: the same shape one level up, with the bounds kept as OBJECTS. Kotlin
            // declares this `rangeTo` on `Comparable<T>`, so the receiver is whatever the program
            // is ordering and the type it RETURNS is what says the element is a reference.
            if comparable_range_element(ret).is_some() && !self.file.declares_its_own_comparable {
                return Some(self.comparable_range_of(receiver, args[0], ret));
            }
        }
        // A member of one. The RECEIVER is what says so, never the owner: `contains` is declared
        // on the facade as well as on the interface, and `start` on the interface a user class may
        // implement.
        if self.type_of(receiver).and_then(floating_range).is_some() {
            let (_, element) = self.type_of(receiver).and_then(floating_range)?;
            let (symbol, answer) = floating_range_symbol(name, args.len())?;
            return Some(self.floating_range_call(symbol, answer, element, receiver, args, ret));
        }
        // A member of a COMPARABLE range, keyed on the receiver for the same reason.
        if comparable_range_element(self.type_of(receiver)?).is_some()
            && !self.file.declares_its_own_comparable
        {
            let (symbol, answer) = comparable_range_symbol(name, args.len())?;
            return Some(self.comparable_range_call(symbol, answer, receiver, args, ret));
        }
        // `a until b` is an extension function of the ranges facade, not a member of the range it
        // answers, so the type it RETURNS is what says which range to build.
        if super::super::super::intrinsics::is_range_until(owner, name, args.len()) {
            let (kind, element) = range_type(ret)?;
            return Some(self.range_of(
                &format!("kt_{kind}_range_until"),
                element,
                ret,
                receiver,
                args[0],
            ));
        }
        // `a downTo b` — `until`'s descending counterpart, and the same shape: an extension of the
        // facade, so the type it RETURNS says which range to build.
        if super::super::super::intrinsics::is_range_down_to(owner, name, args.len()) {
            let (kind, element) = range_type(ret)?;
            return Some(self.range_of(
                &format!("kt_{kind}_range_down_to"),
                element,
                ret,
                receiver,
                args[0],
            ));
        }
        // `range step n` and `progression.reversed()`. Both answer a PROGRESSION, which this
        // runtime represents as the range it already had with a step beside it — so the receiver
        // carries its own width and there is nothing to pick from the result type. The step
        // crosses as a `Long` because every bound here is kept at 64 bits.
        if super::super::super::intrinsics::is_range_step(owner, name, args.len()) {
            // The ANSWER is the progression, a reference; the ARGUMENT is the step, which crosses
            // at the runtime's 64 bits like every other bound. `Long` as the argument type serves
            // both widths: a step is never a `Char`, and an `Int` one widens into it.
            return Some(self.range_call("kt_range_step", any(), Ty::Long, receiver, args, ret));
        }
        if super::super::super::intrinsics::is_range_reversed(owner, name, args.len()) {
            return Some(self.range_call("kt_range_reversed", any(), any(), receiver, &[], ret));
        }
        if self.type_of(receiver).is_some_and(is_range_iterator) {
            let (symbol, carried) = range_iterator_symbol(name, args.len())?;
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
            && args.len() == 1
            && range_element(crate::types::type_name(owner)).is_none()
            && self
                .type_of(receiver)
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
        if let Some(element) = self.type_of(receiver).and_then(closed_range_element) {
            let (symbol, carried) = range_symbol(name, args.len())?;
            return Some(self.closed_range_call(symbol, carried, element, receiver, args, ret));
        }
        let element = range_element(crate::types::type_name(owner))?;
        let (symbol, carried) = range_symbol(name, args.len())?;
        Some(self.range_call(symbol, carried, element, receiver, args, ret))
    }

    /// `a..b` on a receiver ordered by `Comparable`: the range object, its bounds kept as objects.
    fn comparable_range_of(
        &mut self,
        start: u32,
        end: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let first = self.reference(start)?;
        if self.terminated {
            return Ok(None);
        }
        let last = self.reference(end)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_comparable_range", &[any(), any()], ret, &[first, last])
    }

    /// One member of a comparable range. Every operand crosses as a REFERENCE, which is what the
    /// bounds are: the comparison is the value's own `compareTo`, read from its descriptor.
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
        let Some(receiver_ty) = self.type_of(receiver).map(Ty::non_null) else {
            return Err("`contains` on a receiver with no known type".to_string());
        };
        let Some(internal) = receiver_ty.obj_internal() else {
            return Err(format!("`contains` on a `{receiver_ty:?}`"));
        };
        if range_element(internal).is_none() {
            return Err(format!("`contains` on a `{receiver_ty:?}`"));
        }
        let Some(argument_ty) = self.type_of(argument) else {
            return Err("`contains` of a value with no known type".to_string());
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
            return Err(format!("`contains` of a `{argument_ty:?}`"));
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

    /// `for (i in 1u..5u)` — a counted loop the checker left as its BOUNDS rather than as a range.
    ///
    /// Common lowering turns a SIGNED counted loop into a plain `while` before this backend sees
    /// it; an unsigned one arrives here instead, because every comparison and the step itself have
    /// to be read on the unsigned ring. Nothing is constructed: the node carries the two ends, and
    /// the loop is the ordinary header/body/update/exit graph over a counter local.
    ///
    /// The correctness trap is the step. A closed range stops AT its last element, by asking
    /// whether the counter has REACHED the end before advancing it — not by advancing and
    /// comparing, which on `UInt.MAX_VALUE` wraps to zero and starts the walk over. A half-open
    /// range needs no such guard: its header already stops one short of the end, so the counter
    /// never leaves the type.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn counted_loop(
        &mut self,
        variable: u32,
        counter: Ty,
        operation: FirRangeOperation,
        start: u32,
        end: u32,
        body: u32,
        label: String,
    ) -> Result<(), Unsupported> {
        let counter = counter.non_null();
        if carrier(counter).clif().is_none() {
            return Err(format!("a counted loop over `{counter:?}`"));
        }
        // Both ends are evaluated ONCE, in source order, before the loop runs: `a..b` builds a
        // range before anything walks it, so a body that assigns to what named an end cannot move
        // it, and an end with an effect has that effect exactly once even on an empty range.
        let Some(start) = self.coerce(start, counter)? else {
            return Err("a `Unit` range bound".to_string());
        };
        if self.terminated {
            return Ok(());
        }
        let Some(end) = self.coerce(end, counter)? else {
            return Err("a `Unit` range bound".to_string());
        };
        if self.terminated {
            return Ok(());
        }

        let slot = self.declare_value(variable, counter)?;
        self.builder.def_var(slot, start);

        let header = self.builder.create_block();
        let body_block = self.builder.create_block();
        let update_block = self.builder.create_block();
        let exit = self.builder.create_block();
        self.builder.ins().jump(header, &[]);

        // `Char` and the unsigned integers read their top bit as a value; every other counter is a
        // signed number. A checked range loop only ever arrives unsigned today, but the counter is
        // what decides that, not the fact that it arrived.
        let signed = !counter.is_unsigned() && counter != Ty::Char;
        let entry = match (operation, signed) {
            (FirRangeOperation::Through, true) => IntCC::SignedLessThanOrEqual,
            (FirRangeOperation::Through, false) => IntCC::UnsignedLessThanOrEqual,
            (FirRangeOperation::OpenEnd | FirRangeOperation::Until, true) => IntCC::SignedLessThan,
            (FirRangeOperation::OpenEnd | FirRangeOperation::Until, false) => {
                IntCC::UnsignedLessThan
            }
            (FirRangeOperation::DownTo, true) => IntCC::SignedGreaterThanOrEqual,
            (FirRangeOperation::DownTo, false) => IntCC::UnsignedGreaterThanOrEqual,
        };
        self.continue_in(header);
        let current = self.builder.use_var(slot);
        let inside = self.builder.ins().icmp(entry, current, end);
        self.builder.ins().brif(inside, body_block, &[], exit, &[]);

        self.loops.push(LoopFrame {
            label: Some(label),
            break_block: exit,
            continue_block: update_block,
            broken: false,
        });
        self.continue_in(body_block);
        self.statement(body)?;
        if !self.terminated {
            self.builder.ins().jump(update_block, &[]);
        }

        self.continue_in(update_block);
        let closed = !matches!(
            operation,
            FirRangeOperation::OpenEnd | FirRangeOperation::Until
        );
        if closed {
            let step_block = self.builder.create_block();
            let current = self.builder.use_var(slot);
            let last = self.builder.ins().icmp(IntCC::Equal, current, end);
            self.builder.ins().brif(last, exit, &[], step_block, &[]);
            self.continue_in(step_block);
        }
        let current = self.builder.use_var(slot);
        let stepped = if operation == FirRangeOperation::DownTo {
            self.builder.ins().iadd_imm_s(current, -1)
        } else {
            self.builder.ins().iadd_imm_s(current, 1)
        };
        self.builder.def_var(slot, stepped);
        self.builder.ins().jump(header, &[]);
        self.loops.pop();

        // The header always has a false edge, so the exit is always reached from somewhere.
        self.continue_in(exit);
        Ok(())
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
        if carrier(counter).clif().is_none() {
            return Err(format!("a range membership test over `{counter:?}`"));
        }
        let Some(value) = self.coerce(value, counter)? else {
            return Err("a `Unit` value in a range test".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(start) = self.coerce(start, counter)? else {
            return Err("a `Unit` range bound".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let Some(end) = self.coerce(end, counter)? else {
            return Err("a `Unit` range bound".to_string());
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
        let Some(property) = self.file.provider.external_property(target) else {
            return false;
        };
        let Some(getter) = self.file.provider.external_callable(property.getter) else {
            return false;
        };
        // The OWNER comes from the accessor, which is a type identity and not a spelling; the NAME
        // comes from the property, which is the only thing that knows what it is called.
        super::super::super::intrinsics::is_indices(&getter.callable.owner.render(), &property.name)
    }

    /// `x.indices` — `0..size - 1` of an indexable receiver.
    ///
    /// Only a receiver whose size this generator can read qualifies: an array, or a `String`. The
    /// same extension property covers `Collection`, which needs a `size` there is no collection
    /// runtime to answer yet, so one of those declines by receiver rather than by name.
    pub(super) fn indices(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let Some(ty) = self.type_of(receiver).map(Ty::non_null) else {
            return Err("`indices` of a receiver with no known type".to_string());
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
        } else if ty
            .obj_internal()
            .is_some_and(super::super::super::intrinsics::is_list_type)
        {
            // Every list the runtime hands out answers its size through one entry point, the same
            // one `size` itself reaches, so `indices` is that size read as a range.
            let value = self.reference(receiver)?;
            if self.terminated {
                return Ok(None);
            }
            self.runtime_call("kt_list_size", &[any()], Ty::Int, &[value])?
        } else {
            return Err(format!("`indices` of a `{ty:?}`"));
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
    ) -> Option<(String, String, Ty)> {
        let property = self.file.provider.external_property(target)?;
        let getter = self.file.provider.external_callable(property.getter)?;
        // An integral range named by a CONCRETE range type is keyed on the owner. Every other
        // shape declares `start` and `endInclusive` on `ClosedRange`, an interface a user class
        // may implement, so there the RECEIVER is what says this is one of the runtime's — and
        // which of the three it is, since the interface is shared.
        if range_element(getter.callable.owner).is_some() {
            range_symbol(&property.name, 0)?;
        } else {
            let ty = self.type_of(receiver)?;
            if floating_range(ty).is_some() {
                floating_range_symbol(&property.name, 0)?;
            } else if comparable_range_element(ty).is_some()
                && !self.file.declares_its_own_comparable
            {
                comparable_range_symbol(&property.name, 0)?;
            } else if closed_range_element(ty).is_some() {
                range_symbol(&property.name, 0)?;
            } else {
                return None;
            }
        }
        Some((
            getter.callable.owner.render(),
            property.name.clone(),
            getter.callable.ret,
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
        if carried != Ty::Long || ret == Ty::Long {
            return Ok(Some(answer));
        }
        // `IntRange.first` is an `Int` and `CharRange.first` is a `Char`: the runtime answered at
        // its own width and the value narrows to the one the program asked for.
        let Some(clif) = carrier(ret).clif() else {
            return Ok(Some(answer));
        };
        Ok(Some(self.builder.ins().ireduce(clif, answer)))
    }
}
