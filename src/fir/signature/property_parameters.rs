//! Source identities for physical property accessor parameters.

use super::*;

impl ResolvedModuleIndex {
    /// Publish the complete source identity contract for both accessors while the property syntax
    /// is still live. An implicit setter has a typed generated role; it is never recovered later
    /// from a missing name or a bodyless accessor declaration.
    pub fn publish_property_parameter_identities<'a>(
        &mut self,
        id: PropertyId,
        parameters: impl IntoIterator<Item = (&'a str, crate::types::ContextParameterKind)>,
        setter_parameter_name: Option<&'a str>,
    ) {
        let parameters = parameters
            .into_iter()
            .map(|(source_name, kind)| ResolvedPropertyContextParameter {
                source_name: source_name.into(),
                kind,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let expected = self
            .property(id)
            .expect("context parameters require a published property")
            .context_parameter_count as usize;
        assert_eq!(
            parameters.len(),
            expected,
            "property context parameters must match its resolved signature"
        );
        assert!(
            self.property_parameter_identities_published.insert(id),
            "property parameter identities may be published only once"
        );
        if !parameters.is_empty() {
            assert!(
                self.property_context_parameters
                    .insert(id, parameters)
                    .is_none(),
                "property context parameters may be published only once"
            );
        }
        let mutable = self
            .property(id)
            .expect("parameter identities require a published property")
            .mutable;
        assert!(
            mutable || setter_parameter_name.is_none(),
            "an immutable property cannot publish a setter parameter"
        );
        if mutable {
            let identity = setter_parameter_name
                .map_or(ResolvedParameterIdentity::PropertySetterValue, |name| {
                    ResolvedParameterIdentity::Source(name.into())
                });
            assert!(
                self.property_setter_parameter_identities
                    .insert(id, identity)
                    .is_none(),
                "a property setter parameter identity may be published only once"
            );
        }
    }

    pub fn property_context_parameter(
        &self,
        property: PropertyId,
        ordinal: u32,
    ) -> Option<&ResolvedPropertyContextParameter> {
        self.property_context_parameters
            .get(&property)?
            .get(ordinal as usize)
    }

    pub fn property_context_parameter_identities(
        &self,
        property: PropertyId,
    ) -> Option<Box<[ResolvedParameterIdentity]>> {
        if !self
            .property_parameter_identities_published
            .contains(&property)
        {
            return None;
        }
        let count = self.property(property)?.context_parameter_count;
        (0..count)
            .map(|ordinal| {
                let parameter = self.property_context_parameter(property, ordinal)?;
                Some(match parameter.kind {
                    crate::types::ContextParameterKind::Named => {
                        ResolvedParameterIdentity::ContextValue {
                            ordinal,
                            source_name: parameter.source_name.clone(),
                        }
                    }
                    crate::types::ContextParameterKind::Anonymous => {
                        ResolvedParameterIdentity::AnonymousContextParameter { ordinal }
                    }
                    crate::types::ContextParameterKind::LegacyReceiver => {
                        ResolvedParameterIdentity::LegacyContextReceiver { ordinal }
                    }
                    crate::types::ContextParameterKind::None => return None,
                })
            })
            .collect::<Option<Vec<_>>>()
            .map(Vec::into_boxed_slice)
    }

    pub fn property_setter_parameter_identity(
        &self,
        property: PropertyId,
    ) -> Option<&ResolvedParameterIdentity> {
        self.property_setter_parameter_identities.get(&property)
    }
}
