//! Declaration-owned marker names, independent of callable entry aliases and local provider IDs.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReferenceDeclaration(String);

impl ReferenceDeclaration {
    pub(super) fn source(module: &str, callable: crate::fir::CallableId) -> Self {
        Self(super::super::symbols::module_function_symbol(
            module, callable,
        ))
    }

    pub(super) fn source_export(
        ir: &crate::ir::IrFile,
        module: &str,
        function: crate::ir::FunId,
    ) -> Result<Option<Self>, &'static str> {
        let mut identities = ir
            .checked_callable_functions
            .iter()
            .filter_map(|(identity, candidate)| (*candidate == function).then_some(*identity));
        let first = identities.next();
        if identities.next().is_some() {
            return Err("a source export has more than one checked callable identity");
        }
        Ok(first.map(|identity| Self::source(module, identity)))
    }

    pub(super) fn dependency(callable: &crate::backend::KlibDependencyCallable) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut name = String::from("kt_klib_reference_");
        for byte in crate::backend::interface_identity::member_identity(callable) {
            name.push(HEX[usize::from(byte >> 4)] as char);
            name.push(HEX[usize::from(byte & 15)] as char);
        }
        Self(name)
    }

    pub(super) fn marker(&self, bound: bool) -> String {
        let role = if bound { "b" } else { "u" };
        format!("kt_refid_{role}_{}", self.0)
    }
}
