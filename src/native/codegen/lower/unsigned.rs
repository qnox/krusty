//! Kotlin's unsigned integers: `UByte`, `UShort`, `UInt`, `ULong`.
//!
//! Each is a value class over a signed primitive, and common lowering erases it to the machine
//! integer it wraps before this backend sees it. That erasure is right about the REPRESENTATION and
//! silent about everything else: `4294967295u` and `-1` are the same 32 bits, so every question
//! whose answer depends on reading them one way rather than the other has to be asked here, on
//! purpose. The carrier keeps the signedness so widening zero-extends; this module answers the
//! members where more than widening is at stake.
//!
//! Three kinds of member, and the difference between them is the whole of the work:
//!
//! * The ones the machine already answers — `plus`, `minus`, `times`, the bitwise operators, `inv`
//!   — where two's complement makes the signed and unsigned results the same bits.
//! * The ones where the machine has both instructions and the choice is the point: `compareTo` and
//!   the orderings pick the unsigned condition, and `div`/`rem` the unsigned helper.
//! * The ones that leave the machine: `toString` reads the bits as the value they stand for, and a
//!   conversion widens by zero-extension rather than by sign.
//!
//! A member NOT listed here declines by name rather than falling back to the signed answer, which
//! is the same reason the whole family was declined before any of it was implemented.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// A member of one of the unsigned types, or `None` when `owner` names none of them.
    pub(super) fn unsigned_member(
        &mut self,
        owner: &str,
        name: &str,
        params: &[Ty],
        ret: Ty,
        receiver: u32,
        args: &[u32],
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let element = super::super::super::intrinsics::unsigned_owner(owner)?;
        Some(self.unsigned_call(element, name, params, ret, receiver, args))
    }

    fn unsigned_call(
        &mut self,
        element: Ty,
        name: &str,
        params: &[Ty],
        ret: Ty,
        receiver: u32,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        // A unary member reads its receiver at the width the RESULT is computed at: Kotlin's
        // `UByte.plus(UByte)` answers a `UInt`, so both operands widen before the operation and the
        // widening is where the zero-extension happens.
        let width = match name {
            // A conversion and `toString` read the receiver at its own width and answer at another.
            "toString" | "hashCode" | "toByte" | "toShort" | "toInt" | "toLong" | "toUByte"
            | "toUShort" | "toUInt" | "toULong" | "toFloat" | "toDouble" => element,
            // A shift's count is an `Int` and only the shifted value is unsigned.
            "shl" | "shr" => element,
            // A comparison answers an `Int` whatever it compares, so the result says nothing about
            // the width. Kotlin declares the mixed forms too (`UInt.compareTo(ULong)`), and those
            // compare at the wider of the two.
            "compareTo" | "equals" => wider(element, params.first().copied()),
            _ => ret.non_null(),
        };
        let Some(left) = self.coerce(receiver, width)? else {
            return Err(format!("a `Unit` receiver of `{name}`"));
        };
        if self.terminated {
            return Ok(None);
        }
        // Every other operand at the same width, except a shift count, which the callee declares.
        let mut operands = Vec::with_capacity(args.len());
        for (argument, declared) in args.iter().zip(params) {
            let ty = if matches!(name, "shl" | "shr") {
                *declared
            } else {
                width
            };
            let Some(value) = self.coerce(*argument, ty)? else {
                return Err(format!("a `Unit` operand of `{name}`"));
            };
            operands.push(value);
        }
        if self.terminated {
            return Ok(None);
        }
        if carrier(width).clif().is_none() {
            return Err(format!("an unsigned `{name}` on a non-scalar"));
        }
        let answer = match (name, operands.as_slice()) {
            // Two's complement: the bits the machine produces are the same either way.
            ("plus", [rhs]) => self.builder.ins().iadd(left, *rhs),
            ("minus", [rhs]) => self.builder.ins().isub(left, *rhs),
            ("times", [rhs]) => self.builder.ins().imul(left, *rhs),
            ("and", [rhs]) => self.builder.ins().band(left, *rhs),
            ("or", [rhs]) => self.builder.ins().bor(left, *rhs),
            ("xor", [rhs]) => self.builder.ins().bxor(left, *rhs),
            ("inv", []) => self.builder.ins().bnot(left),
            ("inc", []) => self.builder.ins().iadd_imm_s(left, 1),
            ("dec", []) => self.builder.ins().iadd_imm_s(left, -1),
            // Kotlin masks a shift count to the operand's width, and an unsigned right shift is a
            // logical one — which is what `ushr` is for a signed operand of the same width.
            ("shl", [bits]) => self.shift("shl", width, left, *bits)?,
            ("shr", [bits]) => self.shift("ushr", width, left, *bits)?,
            // Where the machine offers both and the choice is the point.
            ("div" | "rem", [rhs]) => {
                let symbol = format!("kt_{name}_{}", unsigned_suffix(width)?);
                self.runtime_call(&symbol, &[width, width], width, &[left, *rhs])?
                    .ok_or_else(|| format!("`{name}` answered nothing"))?
            }
            ("compareTo", [rhs]) => {
                // `-1`, `0` or `1` from the two unsigned questions. A comparison answers `1` for
                // true here, not all-ones, so the sign comes from subtracting one from the other
                // rather than from widening either.
                let less = self.builder.ins().icmp(IntCC::UnsignedLessThan, left, *rhs);
                let greater = self
                    .builder
                    .ins()
                    .icmp(IntCC::UnsignedGreaterThan, left, *rhs);
                let less = self.builder.ins().uextend(types::I32, less);
                let greater = self.builder.ins().uextend(types::I32, greater);
                self.builder.ins().isub(greater, less)
            }
            ("equals", [rhs]) => self.builder.ins().icmp(IntCC::Equal, left, *rhs),
            // Off the machine.
            ("toString", []) => {
                let symbol = format!("kt_{}_to_string", unsigned_suffix(width)?);
                return self.runtime_call(&symbol, &[width], Ty::String, &[left]);
            }
            // A conversion is the widening the carrier already describes: unsigned widens by
            // zero-extension, and narrowing keeps the low bits whatever the signedness.
            (
                "hashCode" | "toByte" | "toShort" | "toInt" | "toLong" | "toUByte" | "toUShort"
                | "toUInt" | "toULong",
                [],
            ) => {
                let target = if name == "hashCode" { Ty::Int } else { ret };
                return self.convert(left, Some(width), target);
            }
            _ => {
                return Err(format!(
                    "the unsigned member `{}.{name}`",
                    unsigned_suffix(element)?
                ))
            }
        };
        // A comparison answers an `Int` and `equals` a `Boolean`; everything else already has the
        // width it was computed at.
        self.convert(answer, Some(machine_ty(name, width)), ret)
    }

    /// `a < b` on two unsigned values, which reaches the generator as the frontend's own compare
    /// intrinsic carrying the ERASED operand type — the one thing that cannot say whether the bits
    /// are to be read as a value or as a sign. The receiver's checked type says it instead.
    pub(super) fn unsigned_compare(
        &mut self,
        element: Ty,
        receiver: u32,
        argument: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let other = self.type_of(argument).unwrap_or(element);
        self.unsigned_call(element, "compareTo", &[other], ret, receiver, &[argument])
    }

    /// `"$u"` — the frontend's own name for rendering an unsigned value, so a template never
    /// reaches for the `Any.toString()` of the signed number sharing its bits.
    pub(super) fn unsigned_intrinsic_to_string(
        &mut self,
        source: Ty,
        receiver: u32,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        // `None` for anything that is not one of the four, so the caller names the construct.
        unsigned_suffix(source).ok()?;
        Some(self.unsigned_call(source, "toString", &[], Ty::String, receiver, &[]))
    }

    /// Kotlin masks a shift count to the operand's width; the runtime's helpers do that.
    fn shift(
        &mut self,
        helper: &str,
        width: Ty,
        value: Value,
        bits: Value,
    ) -> Result<Value, Unsupported> {
        // The runtime declares its shifts on `Int` and `Long` only, which is where Kotlin declares
        // the unsigned ones too: there is no `UByte.shl`.
        let signed = match width {
            Ty::UInt => Ty::Int,
            Ty::ULong => Ty::Long,
            other => return Err(format!("a shift of `{other:?}`")),
        };
        let bits = self
            .convert(bits, Some(Ty::Int), Ty::Int)?
            .expect("an `Int`");
        self.runtime_call(
            &format!(
                "kt_{helper}_{}",
                if signed == Ty::Int { "int" } else { "long" }
            ),
            &[signed, Ty::Int],
            signed,
            &[value, bits],
        )?
        .ok_or_else(|| "a shift answered nothing".to_string())
    }
}

/// The wider of an unsigned member's two operands, when the second is unsigned too.
fn wider(element: Ty, other: Option<Ty>) -> Ty {
    let bits = |ty: Ty| carrier(ty).clif().map_or(0, |clif| clif.bits());
    match other {
        Some(other) if other.non_null().is_unsigned() && bits(other.non_null()) > bits(element) => {
            other.non_null()
        }
        _ => element,
    }
}

/// The type a member's machine result already has, before it is converted to the declared one.
fn machine_ty(name: &str, width: Ty) -> Ty {
    match name {
        "compareTo" => Ty::Int,
        "equals" => Ty::Boolean,
        _ => width,
    }
}

/// The runtime's suffix for one of the unsigned types.
fn unsigned_suffix(ty: Ty) -> Result<&'static str, Unsupported> {
    Ok(match ty {
        Ty::UByte => "ubyte",
        Ty::UShort => "ushort",
        Ty::UInt => "uint",
        Ty::ULong => "ulong",
        other => return Err(format!("an unsigned operation at `{other:?}`")),
    })
}
