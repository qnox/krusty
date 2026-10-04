use super::{CallableId, DeclarationNameId, ResolvedModuleIndex, ResolvedParameterIdentity};

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
    const CROSSINLINE: u16 = 1 << 11;
    /// A named context parameter. Distinct from the unset state: a context-prefix slot that never
    /// published a role must stay a failure, not collapse into this role.
    const NAMED_CONTEXT: u16 = 1 << 12;

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

    pub const fn with_crossinline(mut self, enabled: bool) -> Self {
        if enabled {
            self.0 |= Self::CROSSINLINE;
        }
        self
    }

    /// The declaration wrote `crossinline` on this parameter. The argument is still spliced; the
    /// modifier only forbids a non-local return from it, and declaration metadata records it.
    pub const fn is_crossinline(self) -> bool {
        self.0 & Self::CROSSINLINE != 0
    }

    /// The `crossinline`/`noinline` modifier the declaration wrote on this parameter.
    pub const fn inline_modifier(self) -> crate::types::InlineParameterModifier {
        crate::types::InlineParameterModifier::written(
            self.materializes_its_lambda(),
            self.is_crossinline(),
        )
    }

    pub const fn with_context_kind(mut self, kind: crate::types::ContextParameterKind) -> Self {
        self.0 &= !(Self::ANONYMOUS_CONTEXT | Self::LEGACY_CONTEXT_RECEIVER | Self::NAMED_CONTEXT);
        match kind {
            crate::types::ContextParameterKind::Anonymous => self.0 |= Self::ANONYMOUS_CONTEXT,
            crate::types::ContextParameterKind::LegacyReceiver => {
                self.0 |= Self::LEGACY_CONTEXT_RECEIVER
            }
            crate::types::ContextParameterKind::Named => self.0 |= Self::NAMED_CONTEXT,
            crate::types::ContextParameterKind::None => {}
        }
        self
    }

    pub const fn context_kind(self) -> crate::types::ContextParameterKind {
        if self.0 & Self::ANONYMOUS_CONTEXT != 0 {
            crate::types::ContextParameterKind::Anonymous
        } else if self.0 & Self::LEGACY_CONTEXT_RECEIVER != 0 {
            crate::types::ContextParameterKind::LegacyReceiver
        } else if self.0 & Self::NAMED_CONTEXT != 0 {
            crate::types::ContextParameterKind::Named
        } else {
            crate::types::ContextParameterKind::None
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
    pub(crate) fn publish_annotation_constructor_defaults(
        &mut self,
        callable: CallableId,
        defaults: impl IntoIterator<Item = Option<crate::libraries::DefaultValue>>,
    ) {
        let defaults = defaults.into_iter().collect::<Vec<_>>().into_boxed_slice();
        assert!(
            self.annotation_constructor_defaults
                .insert(callable, defaults)
                .is_none(),
            "an annotation constructor may publish its defaults only once"
        );
    }

    pub fn annotation_constructor_defaults(
        &self,
        callable: CallableId,
    ) -> Option<&[Option<crate::libraries::DefaultValue>]> {
        self.annotation_constructor_defaults
            .get(&callable)
            .map(Box::as_ref)
    }

    pub(crate) fn annotation_constructor_callables(&self) -> Vec<CallableId> {
        self.annotation_constructor_defaults
            .keys()
            .copied()
            .collect()
    }

    pub(crate) fn replace_annotation_constructor_defaults(
        &mut self,
        callable: CallableId,
        defaults: impl IntoIterator<Item = Option<crate::libraries::DefaultValue>>,
    ) {
        let defaults = defaults.into_iter().collect::<Vec<_>>().into_boxed_slice();
        let published = self
            .annotation_constructor_defaults
            .get_mut(&callable)
            .expect("annotation constructor defaults are replaced only after publication");
        assert_eq!(
            published.len(),
            defaults.len(),
            "checked annotation defaults keep the published parameter count"
        );
        *published = defaults;
    }

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

    /// Whether a value parameter (context parameters excluded) or the extension receiver of this
    /// callable has a function type.
    pub fn has_function_typed_parameter(&self, callable: CallableId) -> bool {
        self.function_typed_parameter_callables.contains(&callable)
    }

    pub(crate) fn publish_function_typed_parameter(&mut self, callable: CallableId) {
        assert!(
            self.callable(callable).is_some(),
            "a function-typed parameter fact requires a published callable identity"
        );
        self.function_typed_parameter_callables.insert(callable);
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

    /// Exact semantic identity of one logical declaration parameter. Roles come only from typed
    /// header flags published while the provider/source header is live; spellings are payload, not
    /// discriminators.
    pub fn callable_parameter_identity(
        &self,
        callable: CallableId,
        ordinal: u32,
    ) -> Option<ResolvedParameterIdentity> {
        let header = self.callable(callable)?;
        let parameter = self.callable_parameter(callable, ordinal)?;
        let name = self.callable_parameter_name(callable, ordinal)?;
        if ordinal < header.shape.context_parameter_count {
            let kind = parameter.flags.context_kind();
            // An inconsistent context-prefix slot is a frontend failure. `declared(..., None)`
            // is only for an ordinary parameter; using it here would turn the slot into `Source`
            // or `Unnamed` from its spelling.
            if kind == crate::types::ContextParameterKind::None {
                return None;
            }
            return Some(ResolvedParameterIdentity::declared(ordinal, name, kind));
        }
        Some(if parameter.flags.is_property_setter_value() {
            ResolvedParameterIdentity::PropertySetterValue
        } else {
            ResolvedParameterIdentity::declared(
                ordinal,
                name,
                crate::types::ContextParameterKind::None,
            )
        })
    }

    /// Complete physical identity list for a resolved callable. The extension receiver is a typed
    /// callable-shape fact and is inserted at its semantic position; no name is inspected to find it.
    pub fn callable_parameter_identities(
        &self,
        callable: CallableId,
        physical_count: usize,
    ) -> Option<Box<[ResolvedParameterIdentity]>> {
        let shape = self.callable(callable)?.shape;
        let logical_count =
            physical_count.checked_sub(usize::from(shape.extension_receiver.is_some()))?;
        if self.callable_parameter_name_count(callable) != logical_count {
            return None;
        }
        let mut identities = (0..logical_count)
            .map(|ordinal| self.callable_parameter_identity(callable, ordinal as u32))
            .collect::<Option<Vec<_>>>()?;
        if shape.extension_receiver.is_some() {
            identities.insert(
                shape.context_parameter_count as usize,
                ResolvedParameterIdentity::ExtensionReceiver,
            );
        }
        Some(identities.into_boxed_slice())
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
                    if flags.context_kind() == crate::types::ContextParameterKind::LegacyReceiver {
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

    /// Transfer default availability across an already-resolved override edge. Parameter identity,
    /// type, and every other modifier remain owned by the overriding declaration; only the exact
    /// provider's declaration bit is inherited ordinal-for-ordinal.
    pub(crate) fn publish_inherited_callable_defaults(
        &mut self,
        target: CallableId,
        defaults: &[bool],
        provider: super::ResolvedFunctionOverrideTarget,
    ) {
        let parameters = self
            .callable_parameters
            .get_mut(&target)
            .expect("an inherited default target must retain its callable parameters");
        assert_eq!(
            parameters.len(),
            defaults.len(),
            "an exact override edge must preserve semantic parameter arity"
        );
        for (parameter, &inherited) in parameters.iter_mut().zip(defaults) {
            if inherited {
                parameter.flags.0 |= ResolvedValueParameterFlags::DEFAULT;
            }
        }
        if parameters
            .iter()
            .any(|parameter| parameter.flags.has_default())
        {
            self.callable_default_providers.insert(target, provider);
        }
    }

    pub fn callable_default_provider(
        &self,
        callable: CallableId,
    ) -> Option<super::ResolvedFunctionOverrideTarget> {
        self.callable_default_providers.get(&callable).copied()
    }

    pub(crate) fn callable_default_bitmap(&self, callable: CallableId) -> Option<Vec<bool>> {
        self.callable_parameters.get(&callable).map(|parameters| {
            parameters
                .iter()
                .map(|parameter| parameter.flags.has_default())
                .collect()
        })
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

#[cfg(test)]
mod tests {
    use super::ResolvedParameterIdentity;
    use crate::fir::{
        CallableId, DeclarationId, ResolvedCallableShape, ResolvedModuleIndex,
        ResolvedValueParameterFlags,
    };
    use crate::types::{ContextParameterKind, Ty};

    fn publish(
        context_parameter_count: u32,
        parameters: &[(&str, ContextParameterKind)],
    ) -> (ResolvedModuleIndex, CallableId) {
        let mut index = ResolvedModuleIndex::default();
        let declaration = DeclarationId::from_raw(1);
        let callable = CallableId::from_raw(1);
        index
            .publish_signature(
                declaration,
                (0..parameters.len()).map(|_| Ty::Int),
                Ty::Unit,
            )
            .expect("the fixture types are publishable");
        let context_value_count = parameters
            .iter()
            .take(context_parameter_count as usize)
            .filter(|(_, kind)| *kind == ContextParameterKind::Named)
            .count() as u32;
        index.publish_function_shape(
            callable,
            declaration,
            "sample",
            ResolvedCallableShape {
                context_parameter_count,
                context_value_count,
                extension_receiver: None,
            },
            false,
        );
        index.publish_callable_parameters(
            callable,
            parameters.iter().map(|(name, kind)| {
                (
                    *name,
                    ResolvedValueParameterFlags::new(false, false, false, false)
                        .with_context_kind(*kind),
                )
            }),
        );
        (index, callable)
    }

    #[test]
    fn context_prefix_roles_stay_distinct_from_ordinary_parameters() {
        let (index, callable) = publish(
            3,
            &[
                ("named", ContextParameterKind::Named),
                ("_", ContextParameterKind::Anonymous),
                ("_", ContextParameterKind::LegacyReceiver),
                ("value", ContextParameterKind::None),
                ("", ContextParameterKind::None),
            ],
        );
        assert_eq!(
            index.callable_parameter_identities(callable, 5).as_deref(),
            Some(
                &[
                    ResolvedParameterIdentity::ContextValue {
                        ordinal: 0,
                        source_name: "named".into(),
                    },
                    ResolvedParameterIdentity::AnonymousContextParameter { ordinal: 1 },
                    ResolvedParameterIdentity::LegacyContextReceiver { ordinal: 2 },
                    ResolvedParameterIdentity::Source("value".into()),
                    ResolvedParameterIdentity::Unnamed { ordinal: 4 },
                ][..]
            )
        );
    }

    #[test]
    fn a_context_prefix_without_a_published_role_fails_closed() {
        let (index, callable) = publish(
            1,
            &[
                ("slot", ContextParameterKind::None),
                ("value", ContextParameterKind::None),
            ],
        );
        assert_eq!(index.callable_parameter_identity(callable, 0), None);
        assert_eq!(
            index.callable_parameter_identity(callable, 1),
            Some(ResolvedParameterIdentity::Source("value".into()))
        );
        assert_eq!(index.callable_parameter_identities(callable, 2), None);
    }
}
