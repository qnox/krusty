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
pub struct ResolvedValueParameterFlags(u16);

impl ResolvedValueParameterFlags {
    const VARARG: u16 = 1 << 0;
    const DEFAULT: u16 = 1 << 1;
    const PROPERTY: u16 = 1 << 2;
    const MUTABLE_PROPERTY: u16 = 1 << 3;
    const IMPLICIT_INTEGER_COERCION: u16 = 1 << 4;
    const EXACT: u16 = 1 << 5;
    const NO_INFER: u16 = 1 << 6;
    const MATERIALIZED_LAMBDA: u16 = 1 << 7;
    const ANONYMOUS_CONTEXT: u16 = 1 << 8;
    const LEGACY_CONTEXT_RECEIVER: u16 = 1 << 9;
    const PROPERTY_SETTER_VALUE: u16 = 1 << 10;

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

    pub const fn with_materialized_lambda(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::MATERIALIZED_LAMBDA;
        }
        self
    }

    /// The declaration wrote `noinline` on this parameter, so its argument is a real closure with
    /// a local of its own. An ordinary inline function parameter is spliced at each use inside the
    /// body and owns no local at all. The two are indistinguishable by TYPE — both are
    /// function-typed — so an expansion that needs the difference must read it here.
    pub const fn materializes_its_lambda(self) -> bool {
        self.0 & Self::MATERIALIZED_LAMBDA != 0
    }

    pub const fn with_context_kind(mut self, kind: crate::ast::ContextParameterKind) -> Self {
        match kind {
            crate::ast::ContextParameterKind::Anonymous => self.0 |= Self::ANONYMOUS_CONTEXT,
            crate::ast::ContextParameterKind::LegacyReceiver => {
                self.0 |= Self::LEGACY_CONTEXT_RECEIVER
            }
            crate::ast::ContextParameterKind::None | crate::ast::ContextParameterKind::Named => {}
        }
        self
    }

    pub const fn context_kind(self) -> crate::ast::ContextParameterKind {
        if self.0 & Self::ANONYMOUS_CONTEXT != 0 {
            crate::ast::ContextParameterKind::Anonymous
        } else if self.0 & Self::LEGACY_CONTEXT_RECEIVER != 0 {
            crate::ast::ContextParameterKind::LegacyReceiver
        } else {
            crate::ast::ContextParameterKind::Named
        }
    }

    pub const fn with_property_setter_value(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::PROPERTY_SETTER_VALUE;
        }
        self
    }

    pub const fn is_property_setter_value(self) -> bool {
        self.0 & Self::PROPERTY_SETTER_VALUE != 0
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
            .map(|(name, flags)| {
                let name =
                    if flags.context_kind() == crate::ast::ContextParameterKind::LegacyReceiver {
                        ""
                    } else {
                        name
                    };
                ResolvedValueParameterHeader {
                    name: self.intern_declaration_name(name),
                    flags,
                }
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
