//! Function imports keyed by both physical symbol and exact Native ABI.
//!
//! Object modules name an import by symbol, but one symbol cannot acquire two Cranelift
//! signatures merely because two selected declarations claim it. The first declaration fixes the
//! ABI and every later use must agree exactly.

use cranelift_codegen::ir::Signature;
use cranelift_module::FuncId;

use crate::fx_hash::HashMap;

use super::Unsupported;

struct ImportedFunction {
    id: FuncId,
    signature: Signature,
}

#[derive(Default)]
pub(super) struct ImportedFunctions {
    by_symbol: HashMap<String, ImportedFunction>,
}

impl ImportedFunctions {
    pub(super) fn existing(
        &self,
        symbol: &str,
        signature: &Signature,
    ) -> Result<Option<FuncId>, Unsupported> {
        let Some(existing) = self.by_symbol.get(symbol) else {
            return Ok(None);
        };
        validate_signature(symbol, &existing.signature, signature)?;
        Ok(Some(existing.id))
    }

    pub(super) fn insert(&mut self, symbol: &str, id: FuncId, signature: Signature) {
        let previous = self
            .by_symbol
            .insert(symbol.to_owned(), ImportedFunction { id, signature });
        debug_assert!(
            previous.is_none(),
            "an import is inserted only after exact lookup"
        );
    }
}

fn validate_signature(
    symbol: &str,
    existing: &Signature,
    requested: &Signature,
) -> Result<(), Unsupported> {
    if existing == requested {
        return Ok(());
    }
    Err(format!("the symbol `{symbol}` imported with incompatible Native function ABIs").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_codegen::ir::{types, AbiParam};
    use cranelift_codegen::isa::CallConv;

    fn signature(parameter: cranelift_codegen::ir::Type) -> Signature {
        let mut signature = Signature::new(CallConv::SystemV);
        signature.params.push(AbiParam::new(parameter));
        signature.returns.push(AbiParam::new(types::I32));
        signature
    }

    #[test]
    fn one_symbol_cannot_be_imported_with_two_native_abis() {
        let i32_signature = signature(types::I32);
        assert!(validate_signature("fixture_sum", &i32_signature, &i32_signature).is_ok());
        assert_eq!(
            validate_signature("fixture_sum", &i32_signature, &signature(types::I64))
                .expect_err("the second ABI must be rejected")
                .construct(),
            "the symbol `fixture_sum` imported with incompatible Native function ABIs"
        );
    }
}
