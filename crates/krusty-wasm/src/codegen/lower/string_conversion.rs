//! The string a template makes of each operand, which is also `Any?.toString()`.

use super::*;

impl Body<'_> {
    /// Push `id` converted to a non-null `String`, as a string template does.
    pub(super) fn string_of(&mut self, id: u32) -> Result<(), Unsupported> {
        let Some(ty) = self.type_of(id)? else {
            return self.expression(id);
        };
        if is_unit(ty) {
            // `Unit`'s one instance prints as `kotlin.Unit`, whatever carries it.
            self.statement(id)?;
            self.string_constant("kotlin.Unit");
            return Ok(());
        }
        if ty.is_unsigned() {
            return self.unsigned_string_of(id, ty);
        }
        let nullable_scalar = match ty.kind() {
            TyKind::Nullable(inner) => self
                .scalar_string_function(inner)
                .map(|function| (inner, function)),
            _ => None,
        };
        if let Some((inner, function)) = nullable_scalar {
            // A nullable scalar is its box or `null`.
            let value = self.local(REFERENCE);
            self.expression(id)?;
            self.code
                .local_tee(value)
                .ref_is_null()
                .if_(BlockType::Value(STRING));
            self.string_constant("null");
            self.code.else_().local_get(value);
            self.unbox(inner)?;
            // Both arms are strings; the block's type is the nullable carrier, so the result is
            // re-asserted non-null for the concatenation that consumes it.
            self.code.call(function).end().ref_as_non_null();
            return Ok(());
        }
        self.expression(id)?;
        let function = match ty {
            Ty::String | Ty::Null => self.runtime.string_or_null,
            ty if ty == Ty::nullable(Ty::String) => self.runtime.string_or_null,
            ty if self.scalar_string_function(ty).is_some() => {
                self.scalar_string_function(ty).expect("checked above")
            }
            ty if self.class_of_type(ty).is_some() => return self.object_to_string(ty),
            other => {
                return Err(format!(
                    "a string template part of type `{}`",
                    describe(other)
                ));
            }
        };
        self.code.call(function);
        Ok(())
    }

    /// Decimal conversion of an exact non-null unsigned scalar, retaining its semantic width.
    pub(super) fn unsigned_string_of(&mut self, id: u32, source: Ty) -> Result<(), Unsupported> {
        if !source.is_unsigned() || self.type_of(id)? != Some(source) {
            return Err(
                "an unsigned string conversion without its exact non-null scalar source".into(),
            );
        }
        self.expression(id)?;
        if let Some(mask) = source.unsigned_widen_mask() {
            self.code.i32_const(mask).op(AND32);
        }
        self.code.call(if source == Ty::ULong {
            self.runtime.string_of_u64
        } else {
            self.runtime.string_of_u32
        });
        Ok(())
    }

    /// The runtime function printing a `ty` scalar carrier.
    fn scalar_string_function(&self, ty: Ty) -> Option<u32> {
        Some(match ty {
            Ty::Int | Ty::Short | Ty::Byte => self.runtime.string_of_i32,
            Ty::Long => self.runtime.string_of_i64,
            Ty::Char => self.runtime.string_of_char,
            Ty::Boolean => self.runtime.string_of_boolean,
            _ => return None,
        })
    }

    fn string_constant(&mut self, text: &str) {
        let units = text.encode_utf16().collect::<Vec<_>>();
        self.runtime
            .string_constant(self.module, &mut self.code, &units);
    }
}

/// The non-null `Unit` type, in its scalar or its classifier spelling.
fn is_unit(ty: Ty) -> bool {
    ty == Ty::Unit || matches!(ty.kind(), TyKind::Obj(name, _) if name == krusty::types::wk::unit())
}

#[cfg(test)]
mod tests;
