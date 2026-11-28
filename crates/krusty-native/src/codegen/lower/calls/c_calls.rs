//! Direct C calls authenticated by a Native interop KLIB provider.
//!
//! The provider publishes the physical symbol only after joining an exact interop archive,
//! qualified annotation application, and external declaration. This boundary owns the remaining
//! representation check: only Kotlin values whose Native machine carrier is already the C ABI
//! carrier may cross it.

use super::*;

/// The value a call carries ahead of its arguments, with the type its declaration gives it when
/// that is not the class it is an instance of (the receiver of a numeric extension).
#[derive(Clone, Copy)]
pub(super) struct CCallReceiver {
    pub(super) value: u32,
    pub(super) declared: Option<Ty>,
}

impl BodyLowering<'_, '_, '_> {
    pub(super) fn direct_c_call(
        &mut self,
        function: &krusty::libraries::DirectCFunction,
        params: &[Ty],
        ret: Ty,
        defaults: &[u32],
        receiver: Option<CCallReceiver>,
        args: &[u32],
    ) -> Result<Option<Value>, Unsupported> {
        let symbol = function.symbol();
        if !is_c_identifier(symbol) {
            return Err(declined!(
                "a call of a C function with no callable symbol (`{symbol}`)"
            ));
        }
        if !defaults.is_empty() {
            return Err(declined!(
                "a C function `{symbol}` called with omitted arguments"
            ));
        }
        if params.len() != args.len() {
            return Err(declined!("a call of the variadic C function `{symbol}`"));
        }
        match function.abi() {
            krusty::libraries::CFunctionAbi::Interop => {
                if receiver.is_some() {
                    return Err(declined!("a C function `{symbol}` called with a receiver"));
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
            krusty::libraries::CFunctionAbi::Runtime => {
                // The runtime's C functions take a number as the number it is: a `Double` crosses
                // as a double, an object as the pointer it is. The declaration's own types say
                // which, so a receiver of a numeric extension (`Double.pow`) is not boxed first.
                let params: Vec<Ty> = params.iter().map(|ty| ty.canonical_semantic()).collect();
                let ret = ret.canonical_semantic();
                let mut arguments = Vec::with_capacity(args.len() + 1);
                let mut carried = Vec::with_capacity(args.len() + 1);
                if let Some(CCallReceiver {
                    value: receiver,
                    declared,
                }) = receiver
                {
                    let receiver_ty = declared
                        .map(|ty| ty.canonical_semantic())
                        .filter(|ty| !matches!(self.carrier(*ty), Carrier::Ref));
                    match receiver_ty {
                        Some(ty) => {
                            let Some(value) = self.coerce(receiver, ty)? else {
                                return Ok(None);
                            };
                            arguments.push(value);
                            carried.push(ty);
                        }
                        None => {
                            arguments.push(self.reference(receiver)?);
                            carried.push(any());
                        }
                    }
                }
                carried.extend_from_slice(&params);
                arguments.extend(self.arguments(args, &params)?);
                if self.terminated {
                    return Ok(None);
                }
                self.runtime_call(symbol, &carried, ret, &arguments)
            }
        }
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
