//! Members of a primitive, asked of a value that arrived as an OBJECT.
//!
//! Nothing here is about boxing as a representation — that is `convert`'s business, and it already
//! knows how to put an `Int` in a box and take it out. What is here is the case where the CALL
//! cannot be the ordinary instruction: the receiver reaches the generator as a reference, so the
//! member has to reach the value through the box first.
//!
//! Two shapes, and what separates them is who knows which primitive is inside:
//!
//! * `x++` where `x` is an `Int?`. The frontend selected `Int.inc()`, so the owner says what is in
//!   the box and the step is emitted here after an ordinary unbox.
//! * `n.toInt()` where `n` is a `Number`. The owner says only `Number`, so what is in the box is
//!   the DESCRIPTOR's answer and only the runtime can read it; that half lives in the runtime's
//!   `kt_number_to_*`, reached through `intrinsics::scalar_member`.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// `inc`/`dec` on a primitive reached as an object, or `None` when the member is something
    /// else.
    pub(super) fn boxed_step(
        &mut self,
        owner: &str,
        name: &str,
        params: &[Ty],
        receiver: u32,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let (ty, step) = super::super::super::intrinsics::boxed_step(owner, name, params)?;
        Some(self.step_of(ty, step, receiver))
    }

    fn step_of(&mut self, ty: Ty, step: i64, receiver: u32) -> Result<Option<Value>, Unsupported> {
        // The receiver is taken as the OWNER's type directly rather than as a reference that is
        // then unboxed. A receiver that is already a scalar must not make the round trip: boxing
        // reads the source's own type and unboxing reads the target's, so a disagreement between
        // them — which is exactly what an unmapped `Long.MAX_VALUE` is — comes back as the wrong
        // bits instead of as a decline.
        let Some(value) = self.coerce(receiver, ty)? else {
            if self.terminated {
                return Ok(None);
            }
            return Err(format!("a `Unit` receiver of a `{ty:?}` step"));
        };
        if self.terminated {
            return Ok(None);
        }
        Ok(Some(match ty {
            // `Float.inc()` and `Double.inc()` step by one in their own arithmetic; an integer
            // step is the machine's add, which wraps in the operand's own width exactly as
            // Kotlin's `Byte.inc()` does at 127.
            Ty::Float => {
                let one = self.builder.ins().f32const(step as f32);
                self.builder.ins().fadd(value, one)
            }
            Ty::Double => {
                let one = self.builder.ins().f64const(step as f64);
                self.builder.ins().fadd(value, one)
            }
            _ => self.builder.ins().iadd_imm_s(value, step),
        }))
    }
}
