//! The shape an annotation classifier takes when it is applied or constructed: its elements in
//! order with their positional policy, and the default each element declares.

use super::{LibraryType, ParamList};
use crate::types::Ty;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationParameterPolicy {
    /// Whether positional arguments use ordinary constructor order, are unavailable, or feed the
    /// array-typed `value` element as individual values.
    pub positional: AnnotationPositionalPolicy,
    /// Kotlin declaration `vararg val` materializes an omitted empty array; a classfile annotation
    /// element with a default must remain absent so its declaration default stands.
    pub materialize_omitted_vararg: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationPositionalPolicy {
    Constructor,
    NamedOnly,
    /// The declaration's `value` parameter alone accepts one positional argument.
    Value,
    ValueVararg,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationApplication {
    pub parameters: ParamList,
    pub policy: AnnotationParameterPolicy,
}

/// The default an annotation element declares. A Java source header declares a default it does
/// not evaluate (kotlinc's `JavaMethod.hasAnnotationParameterDefaultValue` with no value): the
/// element may be omitted where it is applied, but no value is known for a construction to take.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnotationElementDefault {
    Evaluated(crate::types::AnnotationValue),
    Unevaluated,
}

impl AnnotationElementDefault {
    pub fn value(&self) -> Option<&crate::types::AnnotationValue> {
        match self {
            Self::Evaluated(value) => Some(value),
            Self::Unevaluated => None,
        }
    }
}

impl LibraryType {
    /// Complete annotation application shape normalized at the declaration-provider boundary.
    /// Kotlin annotation constructors already carry the ordinary positional/default/vararg facts;
    /// providers for constructor-less declaration formats attach an explicit annotation policy.
    pub fn annotation_application(&self) -> Option<AnnotationApplication> {
        if !self.is_annotation() {
            return None;
        }
        let parameters = if let Some(parameters) = self
            .named_parameter_lists
            .iter()
            .find(|parameters| parameters.annotation.is_some())
        {
            if parameters.names.len() != parameters.defaults.len()
                || parameters.names.len() != parameters.types.len()
                || parameters.names.iter().any(String::is_empty)
                || parameters.types.contains(&Ty::Error)
            {
                return None;
            }
            parameters.clone()
        } else {
            self.constructor_named_params(0)?
        };
        let policy = parameters.annotation.unwrap_or(AnnotationParameterPolicy {
            positional: AnnotationPositionalPolicy::Constructor,
            materialize_omitted_vararg: parameters.vararg.is_some(),
        });
        Some(AnnotationApplication { parameters, policy })
    }

    /// The declaration default of the annotation element `name`, when the element declares one.
    pub fn annotation_element_default(&self, name: &str) -> Option<&AnnotationElementDefault> {
        self.annotation_element_defaults
            .iter()
            .find(|(element, _)| &**element == name)
            .map(|(_, value)| value)
    }
}
