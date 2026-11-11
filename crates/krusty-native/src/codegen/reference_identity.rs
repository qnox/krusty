//! Declaration-owned marker names, independent of callable entry aliases and local provider IDs.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReferenceDeclaration(String);

impl ReferenceDeclaration {
    pub(super) fn constructor(module: &str, declaration: krusty::fir::DeclarationId) -> Self {
        Self(super::super::symbols::constructor_reference_symbol(
            module,
            declaration,
        ))
    }

    pub(super) fn property(module: &str, property: krusty::fir::PropertyId) -> Self {
        Self(super::super::symbols::property_reference_symbol(
            module, property,
        ))
    }

    pub(super) fn source(module: &str, callable: krusty::fir::CallableId) -> Self {
        Self(super::super::symbols::module_function_symbol(
            module, callable,
        ))
    }

    pub(super) fn source_export(
        ir: &krusty::ir::IrFile,
        module: &str,
        function: krusty::ir::FunId,
        exports: &krusty::backend::DependencyExportSignatures,
        identity: &krusty::backend::KlibDependencyCallable,
    ) -> Result<Option<Self>, &'static str> {
        let mut identities = ir
            .checked_callable_functions
            .iter()
            .filter_map(|(identity, candidate)| (*candidate == function).then_some(*identity));
        let first = identities.next();
        if identities.next().is_some() {
            return Err("a source export has more than one checked callable identity");
        }
        if let Some(identity) = first {
            return Ok(Some(Self::source(module, identity)));
        }
        // Construction entries export the constructor itself, rather than a source function.
        // Join that exact metadata identity to its checked class/ordinal and declaration ID.
        let mut constructors = exports
            .constructors()
            .iter()
            .filter_map(|(constructor, candidate)| (candidate == identity).then_some(constructor));
        let Some(constructor) = constructors.next() else {
            return Ok(None);
        };
        if constructors.next().is_some() {
            return Err("a source export has more than one checked constructor identity");
        }
        let ordinal = match constructor.secondary {
            None => 0,
            Some(ordinal) => u32::try_from(ordinal)
                .ok()
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or("a source constructor export has an invalid declaration ordinal")?,
        };
        let mut declarations =
            ir.checked_constructor_bodies
                .iter()
                .filter_map(|(declaration, body)| {
                    (body.class == constructor.class && body.ordinal == ordinal)
                        .then_some(*declaration)
                });
        let declaration = declarations
            .next()
            .ok_or("a source constructor export has no checked declaration identity")?;
        if declarations.next().is_some() {
            return Err("a source constructor export has ambiguous checked declaration identities");
        }
        Ok(Some(Self::constructor(module, declaration)))
    }

    pub(super) fn dependency(callable: &krusty::backend::KlibDependencyCallable) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut name = String::from("kt_klib_reference_");
        for byte in krusty::backend::interface_identity::member_identity(callable) {
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
