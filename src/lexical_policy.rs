//! The lexical policies an annotation opens for everything lexically inside what it annotates.
//!
//! `@Suppress("NAME")` silences named diagnostics, and an opt-in marker or `@OptIn(Marker::class)`
//! accepts that marker's requirement (kotlinc's `isExperimentalityAcceptableInContext`). These are
//! the boundary fact between annotation resolution and every later check that enters the annotated
//! element: an application's policies are derived once from its checked application, as resolved
//! classifier identities and constant names, and a policy push never consults source spelling.

use crate::types::{type_name, AnnotationValue, TypeName};

/// One policy in force at a checking position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LexicalPolicy {
    /// Diagnostics with this name are not reported.
    Suppress(String),
    /// An enclosing element is annotated with this classifier, or opts in to it with
    /// `@OptIn(classifier::class)`. Either accepts the opt-in requirement of a marker classifier.
    OptIn(TypeName),
}

impl LexicalPolicy {
    /// The policies of one checked application of `annotation` whose folded arguments are
    /// `values`. Every annotation accepts itself as a marker; `@OptIn` also accepts each class its
    /// arguments name, and `@Suppress` silences each name its arguments hold.
    pub fn of_application(annotation: TypeName, values: &[(String, AnnotationValue)]) -> Vec<Self> {
        let mut policies = vec![Self::OptIn(annotation)];
        let opt_in = annotation == type_name("kotlin/OptIn");
        let suppress = annotation == type_name("kotlin/Suppress");
        if opt_in || suppress {
            for (_, value) in values {
                collect_argument_policies(value, opt_in, &mut policies);
            }
        }
        policies
    }
}

fn collect_argument_policies(value: &AnnotationValue, opt_in: bool, out: &mut Vec<LexicalPolicy>) {
    match value {
        AnnotationValue::Array(elements) => {
            for element in elements {
                collect_argument_policies(element, opt_in, out);
            }
        }
        AnnotationValue::Class(ty) if opt_in => {
            if let Some(marker) = ty.kotlin_class_internal() {
                out.push(LexicalPolicy::OptIn(marker));
            }
        }
        AnnotationValue::String(name) if !opt_in => {
            out.push(LexicalPolicy::Suppress(name.to_lossy()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn an_opt_in_accepts_itself_and_every_named_marker() {
        let policies = LexicalPolicy::of_application(
            type_name("kotlin/OptIn"),
            &[(
                "markerClass".to_string(),
                AnnotationValue::Array(vec![
                    AnnotationValue::Class(Ty::obj("p/A")),
                    AnnotationValue::Class(Ty::obj("p/B")),
                ]),
            )],
        );
        assert_eq!(
            policies,
            vec![
                LexicalPolicy::OptIn(type_name("kotlin/OptIn")),
                LexicalPolicy::OptIn(type_name("p/A")),
                LexicalPolicy::OptIn(type_name("p/B")),
            ]
        );
    }

    #[test]
    fn a_suppress_silences_each_named_diagnostic() {
        let policies = LexicalPolicy::of_application(
            type_name("kotlin/Suppress"),
            &[(
                "names".to_string(),
                AnnotationValue::Array(vec![
                    AnnotationValue::string("A"),
                    AnnotationValue::string("B"),
                ]),
            )],
        );
        assert_eq!(
            policies,
            vec![
                LexicalPolicy::OptIn(type_name("kotlin/Suppress")),
                LexicalPolicy::Suppress("A".to_string()),
                LexicalPolicy::Suppress("B".to_string()),
            ]
        );
    }

    #[test]
    fn another_annotation_opens_only_its_own_marker() {
        let policies = LexicalPolicy::of_application(
            type_name("p/Marker"),
            &[("value".to_string(), AnnotationValue::Class(Ty::obj("p/A")))],
        );
        assert_eq!(policies, vec![LexicalPolicy::OptIn(type_name("p/Marker"))]);
    }
}
