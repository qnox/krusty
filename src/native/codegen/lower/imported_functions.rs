//! Function imports keyed by both physical symbol and exact Native ABI.
//!
//! Object modules name an import by symbol, but one symbol cannot acquire two Cranelift
//! signatures merely because two selected declarations claim it. The linker joins every object's
//! import of a symbol into one untyped definition, so the first declaration anywhere in the
//! emission fixes the ABI and every later use, in any file, must agree exactly.

use cranelift_codegen::ir::Signature;
use cranelift_module::FuncId;

use crate::fx_hash::HashMap;

use super::Unsupported;

/// The ABI each imported symbol was first declared with, for the whole emission session.
#[derive(Default)]
pub(in crate::native::codegen) struct ImportedAbis {
    by_symbol: HashMap<String, Signature>,
}

/// One object's imported functions, checked against the emission-wide ABIs.
pub(super) struct ImportedFunctions<'a> {
    abis: &'a mut ImportedAbis,
    ids: HashMap<String, FuncId>,
}

impl<'a> ImportedFunctions<'a> {
    pub(super) fn new(abis: &'a mut ImportedAbis) -> Self {
        Self {
            abis,
            ids: HashMap::default(),
        }
    }

    pub(super) fn existing(
        &self,
        symbol: &str,
        signature: &Signature,
    ) -> Result<Option<FuncId>, Unsupported> {
        if let Some(existing) = self.abis.by_symbol.get(symbol) {
            validate_signature(symbol, existing, signature)?;
        }
        Ok(self.ids.get(symbol).copied())
    }

    pub(super) fn insert(&mut self, symbol: &str, id: FuncId, signature: Signature) {
        let previous = self.ids.insert(symbol.to_owned(), id);
        debug_assert!(
            previous.is_none(),
            "an import is inserted only after exact lookup"
        );
        self.abis
            .by_symbol
            .entry(symbol.to_owned())
            .or_insert(signature);
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

    #[test]
    fn a_later_object_cannot_import_a_symbol_with_another_abi() {
        let mut abis = ImportedAbis::default();
        let mut first = ImportedFunctions::new(&mut abis);
        assert_eq!(
            first
                .existing("fixture_sum", &signature(types::I32))
                .unwrap(),
            None
        );
        first.insert("fixture_sum", FuncId::from_u32(0), signature(types::I32));
        let second = ImportedFunctions::new(&mut abis);
        assert_eq!(
            second
                .existing("fixture_sum", &signature(types::I32))
                .unwrap(),
            None
        );
        assert_eq!(
            second
                .existing("fixture_sum", &signature(types::I64))
                .expect_err("another object's ABI must be rejected")
                .construct(),
            "the symbol `fixture_sum` imported with incompatible Native function ABIs"
        );
    }
}
