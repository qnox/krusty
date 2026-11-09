//! Direct C calls authenticated by a Native interop KLIB provider.
//!
//! The provider publishes the physical symbol only after joining an exact interop archive,
//! qualified annotation application, and external declaration. This boundary owns the remaining
//! representation check: only Kotlin values whose Native machine carrier is already the C ABI
//! carrier may cross it.

use super::*;

impl BodyLowering<'_, '_, '_> {
    pub(super) fn direct_c_call(
        &mut self,
        symbol: &str,
        params: &[Ty],
        ret: Ty,
        defaults: &[u32],
        dispatch_receiver: Option<u32>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        if !is_c_identifier(symbol) {
            return Err(declined!(
                "a call of a C function with no callable symbol (`{symbol}`)"
            ));
        }
        if dispatch_receiver.is_some() {
            return Err(declined!("a C function `{symbol}` called with a receiver"));
        }
        if !defaults.is_empty() {
            return Err(declined!(
                "a C function `{symbol}` called with omitted arguments"
            ));
        }
        if params.len() != args.len() {
            return Err(declined!("a call of the variadic C function `{symbol}`"));
        }
        for ty in params.iter().chain([&ret]) {
            if matches!(self.carrier(*ty), Carrier::Ref) {
                return Err(declined!(
                    "a call of the C function `{symbol}` passing `{}`, which has no C representation here",
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

fn spelling(ty: Ty) -> String {
    let nullable = if ty.is_nullable() { "?" } else { "" };
    match ty.non_null().obj_internal() {
        Some(name) => format!("{}{nullable}", name.render().replace('/', ".")),
        None => format!("{ty:?}"),
    }
}

fn is_c_identifier(symbol: &str) -> bool {
    let mut characters = symbol.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|rest| rest == '_' || rest.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::is_c_identifier;

    #[test]
    fn only_linkable_c_identifiers_are_accepted() {
        assert!(is_c_identifier("fixture_sum"));
        assert!(is_c_identifier("_fixture2"));
        assert!(!is_c_identifier(""));
        assert!(!is_c_identifier("2fixture"));
        assert!(!is_c_identifier("fixture unsupported: no symbol"));
    }
}
