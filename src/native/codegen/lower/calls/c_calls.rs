//! Direct calls of the C functions an interop library declares.
//!
//! An interop library declares each C function as an `external` Kotlin function whose provider
//! published the C symbol it is realized by (`@CCall.Direct(name)`). The call is that symbol's,
//! with the target's C calling convention, which is the convention every runtime call here
//! already uses: an integer narrower than 32 bits is extended by its signedness, as the C ABI
//! requires of the caller. The linker binds the symbol to the shared library that exports it.
//!
//! Only values whose machine form is the C type's are passed: the integers, unsigned integers,
//! floating-point numbers and `Boolean`. A pointer, a struct and a `String` (a `const char *`
//! parameter) need the `kotlinx.cinterop` representation of C data, which this generator does not
//! have yet, so a call passing or returning one declines rather than handing C a collected
//! reference.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Call the C function `symbol` with `args`, whose checked types are `params`, for a result
    /// of type `ret`.
    pub(in super::super) fn direct_c_call(
        &mut self,
        symbol: &str,
        params: &[Ty],
        ret: Ty,
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if !is_c_identifier(symbol) {
            return Err(declined!(
                "a call of a C function with no symbol to call (`{symbol}`)"
            ));
        }
        if dispatch_receiver.is_some() {
            return Err(declined!("a C function `{symbol}` called with a receiver"));
        }
        if params.len() != args.len() {
            return Err(declined!("a call of the variadic C function `{symbol}`"));
        }
        for ty in params.iter().chain([&ret]) {
            if matches!(self.carrier(*ty), Carrier::Ref) {
                return Err(declined!(
                    "a call of the C function `{symbol}` passing `{}`, which has no C \
                     representation here",
                    spelling(*ty)
                ));
            }
        }
        let arguments = self.arguments(args, params)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(symbol, params, ret, &arguments)
    }
}

/// A type as a diagnostic names it.
fn spelling(ty: Ty) -> String {
    let nullable = if ty.is_nullable() { "?" } else { "" };
    match ty.non_null().obj_internal() {
        Some(name) => format!("{}{nullable}", name.render().replace('/', ".")),
        None => format!("{ty:?}"),
    }
}

/// Whether `symbol` names a C function: an identifier, not the explanation cinterop records for
/// a function it cannot bind (`f unsupported: function is defined in a header file`).
fn is_c_identifier(symbol: &str) -> bool {
    let mut characters = symbol.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|rest| rest == '_' || rest.is_ascii_alphanumeric())
}
