use super::{CallableId, DeclarationNameId, ResolvedModuleIndex};

/// Persistent semantic facts for one callable value parameter. Source types are represented by the
/// pending-free `ResolvedSignature`; this compact record carries declaration behavior that the
/// checker and metadata emitter must not recover from reparsed syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedValueParameterHeader {
    name: DeclarationNameId,
    flags: ResolvedValueParameterFlags,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedValueParameterFlags(u8);

impl ResolvedValueParameterFlags {
    const VARARG: u8 = 1 << 0;
    const DEFAULT: u8 = 1 << 1;
    const PROPERTY: u8 = 1 << 2;
    const MUTABLE_PROPERTY: u8 = 1 << 3;
    const IMPLICIT_INTEGER_COERCION: u8 = 1 << 4;
    const EXACT: u8 = 1 << 5;
    const NO_INFER: u8 = 1 << 6;

    pub const fn new(vararg: bool, default: bool, property: bool, mutable_property: bool) -> Self {
        let mut bits = 0;
        if vararg {
            bits |= Self::VARARG;
        }
        if default {
            bits |= Self::DEFAULT;
        }
        if property {
            bits |= Self::PROPERTY;
        }
        if mutable_property {
            bits |= Self::MUTABLE_PROPERTY;
        }
        Self(bits)
    }

    pub const fn is_vararg(self) -> bool {
        self.0 & Self::VARARG != 0
    }

    pub const fn has_default(self) -> bool {
        self.0 & Self::DEFAULT != 0
    }

    pub const fn is_property(self) -> bool {
        self.0 & Self::PROPERTY != 0
    }

    pub const fn is_mutable_property(self) -> bool {
        self.0 & Self::MUTABLE_PROPERTY != 0
    }

    pub const fn with_implicit_integer_coercion(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::IMPLICIT_INTEGER_COERCION;
        }
        self
    }

    pub const fn has_implicit_integer_coercion(self) -> bool {
        self.0 & Self::IMPLICIT_INTEGER_COERCION != 0
    }

    pub const fn with_exact(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::EXACT;
        }
        self
    }

    pub const fn is_exact(self) -> bool {
        self.0 & Self::EXACT != 0
    }

    pub const fn with_no_infer(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::NO_INFER;
        }
        self
    }

    pub const fn is_no_infer(self) -> bool {
        self.0 & Self::NO_INFER != 0
    }
}

/// Compact declaration behavior that affects callable selection but is not part of its semantic
/// parameter/result type. Pass 1 publishes these facts while the provisional source signature is
/// alive; Pass 2 must not reopen a parser declaration or a coordinate-keyed side table to recover
/// them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedCallableBehavior {
    pub requires_splice: bool,
    pub projected_return_hazard: bool,
    pub plugin_expression: Option<crate::libraries::PluginExpressionDeclaration>,
}

impl ResolvedValueParameterHeader {
    pub const fn flags(self) -> ResolvedValueParameterFlags {
        self.flags
    }
}

impl ResolvedModuleIndex {
    pub fn callable_behavior(&self, callable: CallableId) -> ResolvedCallableBehavior {
        self.callable_behaviors
            .get(&callable)
            .copied()
            .unwrap_or_default()
    }

    pub fn publish_callable_behavior(
        &mut self,
        callable: CallableId,
        behavior: ResolvedCallableBehavior,
    ) {
        assert!(
            self.callable(callable).is_some(),
            "callable behavior requires a published callable identity"
        );
        assert!(
            self.callable_behaviors.insert(callable, behavior).is_none(),
            "a callable may publish behavior only once"
        );
    }

    /// Strict-equality refinement published by `equals`' first ordinary value parameter.
    pub fn callable_equality_bound(&self, callable: CallableId) -> Option<super::ResolvedTy> {
        self.callable_equality_bounds.get(&callable).copied()
    }

    pub fn publish_callable_equality_bound(
        &mut self,
        callable: CallableId,
        bound: crate::types::Ty,
    ) -> Result<(), super::UnpublishableType> {
        assert!(
            self.callable(callable).is_some(),
            "an equality bound requires a published callable identity"
        );
        let bound = super::ResolvedTy::new(bound)?;
        assert!(
            self.callable_equality_bounds
                .insert(callable, bound)
                .is_none(),
            "a callable may publish only one equality bound"
        );
        Ok(())
    }

    pub fn callable_parameter(
        &self,
        callable: CallableId,
        ordinal: u32,
    ) -> Option<ResolvedValueParameterHeader> {
        self.callable_parameters
            .get(&callable)?
            .get(ordinal as usize)
            .copied()
    }

    pub fn callable_parameter_name(&self, callable: CallableId, ordinal: u32) -> Option<&str> {
        let parameter = self.callable_parameter(callable, ordinal)?;
        self.declaration_names
            .get(parameter.name.raw() as usize)
            .map(AsRef::as_ref)
    }

    pub fn callable_parameter_name_count(&self, callable: CallableId) -> usize {
        self.callable_parameters
            .get(&callable)
            .map_or(0, |parameters| parameters.len())
    }

    pub fn publish_callable_parameters<'a>(
        &mut self,
        callable: CallableId,
        parameters: impl IntoIterator<Item = (&'a str, ResolvedValueParameterFlags)>,
    ) {
        assert!(
            self.callable(callable).is_some(),
            "parameters require a published callable identity"
        );
        let parameters = parameters
            .into_iter()
            .map(|(name, flags)| ResolvedValueParameterHeader {
                name: self.intern_declaration_name(name),
                flags,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        assert!(
            self.callable_parameters
                .insert(callable, parameters)
                .is_none(),
            "a callable may publish parameter facts only once"
        );
    }

    pub(super) fn callable_parameter_storage_payload_bytes(&self) -> usize {
        self.callable_parameters.len()
            * (std::mem::size_of::<CallableId>()
                + std::mem::size_of::<Box<[ResolvedValueParameterHeader]>>())
            + self
                .callable_parameters
                .values()
                .map(|parameters| {
                    parameters.len() * std::mem::size_of::<ResolvedValueParameterHeader>()
                })
                .sum::<usize>()
            + self.callable_equality_bounds.len()
                * (std::mem::size_of::<CallableId>() + std::mem::size_of::<super::ResolvedTy>())
            + self.callable_behaviors.len()
                * (std::mem::size_of::<CallableId>()
                    + std::mem::size_of::<ResolvedCallableBehavior>())
    }
}
