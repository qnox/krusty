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
/// read of one names that owner. Listing the progressions here is sound because nothing else can
/// produce one: `step` and `downTo` are declined, so the only progression a lowered program holds
/// is a range. The two members a range gets from `ClosedRange` — `start` and `endInclusive` — are
/// overridden by each concrete range and so arrive under its own name; the interface is left out,
/// because a user class may implement it and would not be one of these objects.
fn range_element(owner: TypeName) -> Option<Ty> {
    [
        ("kotlin/ranges/IntRange", Ty::Int),
        ("kotlin/ranges/IntProgression", Ty::Int),
        ("kotlin/ranges/LongRange", Ty::Long),
        ("kotlin/ranges/LongProgression", Ty::Long),
        ("kotlin/ranges/CharRange", Ty::Char),
        ("kotlin/ranges/CharProgression", Ty::Char),
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
    ]
    .into_iter()
    .find_map(|(candidate, kind, element)| internal.matches(candidate).then_some((kind, element)))
}

/// The runtime function answering one range member, and the width it answers at. Every bound is
/// kept at 64 bits, so a member that answers one is a truncation of what the runtime returns.
fn range_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("contains", 1) => ("kt_range_contains", Ty::Boolean),
        ("isEmpty", 0) => ("kt_range_is_empty", Ty::Boolean),
        ("getFirst" | "getStart", 0) => ("kt_range_first", Ty::Long),
        ("getLast" | "getEndInclusive", 0) => ("kt_range_last", Ty::Long),
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
    ]
    .iter()
    .any(|candidate| internal.matches(candidate))
}

/// The runtime function answering one member of a range's iterator.
fn range_iterator_symbol(name: &str, arity: usize) -> Option<(&'static str, Ty)> {
    Some(match (name, arity) {
        ("hasNext", 0) => ("kt_range_iterator_has_next", Ty::Boolean),
        ("next" | "nextInt" | "nextLong" | "nextChar", 0) => ("kt_range_iterator_next", Ty::Long),
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
        let Some((kind, element)) = element(start_type.non_null(), end_type.non_null()) else {
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
        if self.type_of(receiver).is_some_and(is_range_iterator) {
            let (symbol, carried) = range_iterator_symbol(name, args.len())?;
            // The element width the iterator answers at is the one the loop variable has.
            return Some(self.range_call(symbol, carried, ret.non_null(), receiver, args, ret));
        }
        let element = range_element(crate::types::type_name(owner))?;
        let (symbol, carried) = range_symbol(name, args.len())?;
        Some(self.range_call(symbol, carried, element, receiver, args, ret))
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
        let Some(property) = self.file.classpath.external_property(target) else {
            return false;
        };
        let Some(getter) = self.file.classpath.external_callable(property.getter) else {
            return false;
        };
        super::super::super::intrinsics::is_indices(
            &getter.callable.owner.render(),
            &getter.callable.name,
        )
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
        let size = if ty.is_array() {
            self.array_size(receiver)?
        } else if ty == Ty::String {
            let value = self.reference(receiver)?;
            if self.terminated {
                return Ok(None);
            }
            self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])?
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
    ) -> Option<(String, String, Ty)> {
        let property = self.file.classpath.external_property(target)?;
        let getter = self.file.classpath.external_callable(property.getter)?;
        range_element(getter.callable.owner)?;
        range_symbol(&getter.callable.name, 0)?;
        Some((
            getter.callable.owner.render(),
            getter.callable.name.clone(),
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
            // `Char` is unsigned and the other two are signed; `coerce` to the element type and the
            // widening to the runtime's 64 bits carries that signedness with it.
            let Some(value) = self.coerce(*argument, element)? else {
                return Ok(None);
            };
            let widened = self.resize(
                value,
                self.builder.func.dfg.value_type(value),
                element != Ty::Char,
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
