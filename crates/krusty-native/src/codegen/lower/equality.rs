//! `equals` and `hashCode` of values already lowered: the rules Kotlin fixes for each primitive,
//! a value class's projection onto the value it holds, and a data class's generated members.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// One field's contribution to a data class's `hashCode`, which is the field's own `hashCode`,
    /// except that an ARRAY field contributes its contents' hash, as kotlinc's does.
    ///
    /// Kotlin's answer for each primitive is fixed, and these are those answers rather than
    /// anything this generator is free to choose: a program can print a hash, and two programs
    /// that agree on everything else must agree on it. `Boolean` is 1231 or 1237 — arbitrary, and
    /// arbitrary in the same way everywhere. `Long` folds its halves together so the high word is
    /// not lost in the truncation to `Int`, and `Double` does the same to its bits. A `Float` is
    /// its bits. The smaller integers are themselves, widened.
    pub(super) fn field_hash(&mut self, value: u32, ty: Ty) -> Result<Option<Value>, Unsupported> {
        if self.carrier(ty) == Carrier::Ref {
            // Including a nullable primitive, which is a box and hashes through its own type.
            let value = self.reference(value)?;
            if self.terminated {
                return Ok(None);
            }
            return self.generated_property_hash(value, ty).map(Some);
        }
        let Some(operand) = self.coerce(value, ty)? else {
            return Err("a `Unit` data-class field".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.value_hash(operand, ty).map(Some)
    }

    /// The hash a generated `hashCode` takes of a property value already in hand. An array
    /// property hashes by its CONTENTS (`Arrays.hashCode`, which is `contentHashCode()`), not by
    /// the array's own identity `hashCode`; every other property hashes as [`Self::value_hash`]
    /// says. Equality of the same property stays the array's identity.
    pub(super) fn generated_property_hash(
        &mut self,
        operand: Value,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        if !ty.non_null().is_array() {
            return self.value_hash(operand, ty);
        }
        Ok(self
            .runtime_call("kt_array_content_hash_code", &[any()], Ty::Int, &[operand])?
            .expect("`kt_array_content_hash_code` returns an Int"))
    }

    /// The hash of a value already in hand, by the same rules.
    pub(super) fn value_hash(&mut self, operand: Value, ty: Ty) -> Result<Value, Unsupported> {
        // A value class hashes as the value it holds, by the rule for THAT value's type.
        let ty = self.file.values.project(ty);
        if self.carrier(ty) == Carrier::Ref {
            return Ok(self
                .runtime_call("kt_hash_code", &[any()], Ty::Int, &[operand])?
                .expect("`kt_hash_code` returns an Int"));
        }
        let hash = match ty.non_null() {
            Ty::Boolean => {
                let yes = self.builder.ins().iconst(types::I32, 1231);
                let no = self.builder.ins().iconst(types::I32, 1237);
                let zero = self.builder.ins().iconst(types::I8, 0);
                let set = self.builder.ins().icmp(IntCC::NotEqual, operand, zero);
                self.builder.ins().select(set, yes, no)
            }
            // An unsigned integer hashes as the SIGNED value it wraps, because that is what
            // Kotlin declares: `UByte` and `UShort` answer `data.toInt()` on their `Byte`/`Short`,
            // which sign-extends, `UInt` answers its `Int` unchanged, and `ULong` folds its `Long`
            // exactly as `Long` does. Nothing here may choose differently — a program can print a
            // hash, and two programs that agree on everything else must agree on it.
            Ty::Byte | Ty::Short | Ty::UByte | Ty::UShort => {
                self.builder.ins().sextend(types::I32, operand)
            }
            Ty::Char => self.builder.ins().uextend(types::I32, operand),
            Ty::Int | Ty::UInt => operand,
            Ty::Long | Ty::ULong => self.fold_to_int(operand),
            // A floating-point value hashes by `toBits()`, which is what makes `NaN`'s hash a
            // number at all: `Float` gives its 32 directly, and `Double` folds its 64 exactly as
            // `Long` does. Reading the bits is a reinterpretation, not a conversion, but
            // `toBits()` answers every `NaN` with the one canonical pattern, so the hash does too.
            Ty::Float => self.canonical_bits_of(operand, types::I32),
            Ty::Double => {
                let bits = self.canonical_bits_of(operand, types::I64);
                self.fold_to_int(bits)
            }
            other => return Err(declined!("a data-class field of type `{other:?}`")),
        };
        Ok(hash)
    }

    /// A floating-point value's BITS as an integer of the same width.
    ///
    /// A reinterpretation, not a conversion: a conversion would round, and rounding `NaN` loses the
    /// very distinction the rules that ask for these bits exist to preserve. Cranelift's `bitcast`
    /// takes an endianness rather than the ordinary memory flags — the two widths are the same
    /// register here, so `little` names a reinterpretation with no swap on every target krusty
    /// emits for.
    pub(super) fn bits_of(&mut self, value: Value, clif: Type) -> Value {
        // ONLY the endianness: `bitcast` rejects every other flag, and the alignment and
        // non-trapping bits `trusted` carries are a memory access's business, which this is not.
        let flags = cranelift_codegen::ir::MemFlagsData::new()
            .with_endianness(cranelift_codegen::ir::Endianness::Little);
        self.builder.ins().bitcast(clif, flags, value)
    }

    /// `toBits()`: the value's bits, except that every `NaN` answers the canonical quiet `NaN`
    /// pattern (`0x7fc00000` for a `Float`, `0x7ff8000000000000` for a `Double`).
    pub(super) fn canonical_bits_of(&mut self, value: Value, clif: Type) -> Value {
        let bits = self.bits_of(value, clif);
        let canonical = if clif == types::I32 {
            self.builder.ins().iconst(types::I32, 0x7fc0_0000)
        } else {
            self.builder.ins().iconst(types::I64, 0x7ff8_0000_0000_0000)
        };
        let is_nan = self.builder.ins().fcmp(FloatCC::Unordered, value, value);
        self.builder.ins().select(is_nan, canonical, bits)
    }

    /// `(value xor (value ushr 32)).toInt()` — how Kotlin folds 64 bits into a hash.
    pub(super) fn fold_to_int(&mut self, value: Value) -> Value {
        let shift = self.builder.ins().iconst(types::I64, 32);
        let high = self.builder.ins().ushr(value, shift);
        let folded = self.builder.ins().bxor(value, high);
        self.builder.ins().ireduce(types::I32, folded)
    }

    /// Whether two data-class fields are equal, which is `equals`, not `==` on the machine.
    ///
    /// A reference field is the runtime's null-safe `equals`, which is Kotlin's. A floating-point
    /// field compares by its BITS, which is the opposite of what the comparison instruction
    /// answers in both directions: `NaN` equals `NaN`, and `0.0` does not equal `-0.0`.
    pub(super) fn field_equals(
        &mut self,
        left: u32,
        right: u32,
        ty: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if self.carrier(ty) != Carrier::Ref {
            let left = self.coerce(left, ty)?;
            let right = self.coerce(right, ty)?;
            if self.terminated {
                return Ok(None);
            }
            let (Some(left), Some(right)) = (left, right) else {
                return Err("a `Unit` data-class field".into());
            };
            return self.values_equal(left, right, ty).map(Some);
        }
        let left = self.reference(left)?;
        let right = self.reference(right)?;
        if self.terminated {
            return Ok(None);
        }
        self.values_equal(left, right, ty).map(Some)
    }

    /// Whether two values already in hand are `equals`, by the same rules.
    pub(super) fn values_equal(
        &mut self,
        left: Value,
        right: Value,
        ty: Ty,
    ) -> Result<Value, Unsupported> {
        // A value class compares by the value it holds, by the rule for THAT value's type.
        let ty = self.file.values.project(ty);
        if self.carrier(ty) != Carrier::Ref {
            // `Double.equals` is not `==`: it reads the bits, so `NaN` equals itself and the two
            // zeroes are distinct. Comparing the reinterpreted integers is exactly that rule, and
            // it is the rule Kotlin's own `equals` states — `==` on the machine answers the other
            // way round on both of those values.
            if matches!(ty.non_null(), Ty::Float | Ty::Double) {
                let clif = if ty.non_null() == Ty::Float {
                    types::I32
                } else {
                    types::I64
                };
                let left = self.bits_of(left, clif);
                let right = self.bits_of(right, clif);
                return Ok(self.builder.ins().icmp(IntCC::Equal, left, right));
            }
            return Ok(self.builder.ins().icmp(IntCC::Equal, left, right));
        }
        Ok(self
            .runtime_call("kt_equals", &[any(), any()], Ty::Boolean, &[left, right])?
            .expect("`kt_equals` returns a Boolean"))
    }
}
