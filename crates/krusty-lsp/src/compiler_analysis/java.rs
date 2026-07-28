//! Resolve parsed Java type references to source-set declarations.

use std::collections::HashMap;

use super::navigation::{DefinitionOccurrence, DefinitionTarget};
use krusty::java_source::parse_file;

/// Internal name (`demo/Greeter`) → where it is declared.
pub type ClassTargets = HashMap<String, DefinitionTarget>;

/// Every resolvable type reference in `source`, paired with the declaration it names.
pub fn definition_occurrences(source: &str, classes: &ClassTargets) -> Vec<DefinitionOccurrence> {
    let Some(file) = parse_file(source) else {
        return Vec::new();
    };
    file.references
        .iter()
        .filter_map(|reference| {
            let internal =
                file.resolve_reference(reference, &|candidate| classes.contains_key(candidate))?;
            let target = *classes.get(&internal)?;
            Some(DefinitionOccurrence {
                span: reference.span,
                target,
            })
        })
        .collect()
}

pub fn declared_class_occurrences(
    source: &str,
    file_index: u32,
) -> Vec<(String, DefinitionTarget)> {
    let Some(file) = parse_file(source) else {
        return Vec::new();
    };
    file.declarations
        .iter()
        .map(|declared| {
            (
                declared.internal.clone(),
                DefinitionTarget {
                    file: file_index,
                    span: declared.name_span,
                },
            )
        })
        .collect()
}

pub fn global_declared_class_occurrences(
    source: &str,
    file_index: u32,
) -> Vec<(String, DefinitionTarget)> {
    let Some(file) = parse_file(source) else {
        return Vec::new();
    };
    let private_nested = file
        .declarations
        .iter()
        .filter(|declared| declared.private && declared.internal.contains('$'))
        .map(|declared| declared.internal.as_str())
        .collect::<std::collections::HashSet<_>>();
    declared_class_occurrences(source, file_index)
        .into_iter()
        .filter(|(internal, _)| !private_nested.contains(internal.as_str()))
        .collect()
}

pub fn declared_classes(source: &str, file_index: u32) -> ClassTargets {
    declared_class_occurrences(source, file_index)
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::diag::Span;

    fn target(file: u32, lo: u32, hi: u32) -> DefinitionTarget {
        DefinitionTarget {
            file,
            span: Span::new(lo, hi),
        }
    }

    fn classes(entries: &[(&str, DefinitionTarget)]) -> ClassTargets {
        entries
            .iter()
            .map(|(name, target)| ((*name).to_string(), *target))
            .collect()
    }

    #[test]
    fn same_package_reference_resolves_to_the_declaration() {
        let source = "package demo;\n\nclass Use {\n    Greeter g;\n}\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("demo/Greeter", target(7, 19, 26))]));
        assert_eq!(
            occurrences,
            vec![DefinitionOccurrence {
                span: Span::new(31, 38),
                target: target(7, 19, 26),
            }]
        );
        assert_eq!(&source[31..38], "Greeter");
    }

    #[test]
    fn imported_reference_resolves_across_packages() {
        let source = "package demo;\nimport other.Greeter;\nclass Use { Greeter g; }\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("other/Greeter", target(3, 0, 7))]));
        assert_eq!(occurrences.len(), 2);
        assert!(occurrences
            .iter()
            .all(|occurrence| occurrence.target == target(3, 0, 7)));
    }

    #[test]
    fn single_type_import_wins_over_the_files_own_package() {
        let source = "package demo;\nimport other.Greeter;\nclass Use { Greeter g; }\n";
        let occurrences = definition_occurrences(
            source,
            &classes(&[
                ("other/Greeter", target(3, 0, 7)),
                ("demo/Greeter", target(9, 0, 7)),
            ]),
        );
        assert_eq!(occurrences[0].target, target(3, 0, 7));
    }

    #[test]
    fn fully_qualified_reference_resolves_without_an_import() {
        let source = "package demo;\nclass Use { void m() { new other.Greeter(); } }\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("other/Greeter", target(4, 0, 7))]));
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].target, target(4, 0, 7));
    }

    #[test]
    fn wildcard_import_resolves_when_nothing_more_specific_matches() {
        let source = "package demo;\nimport other.*;\nclass Use { Greeter g; }\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("other/Greeter", target(2, 0, 7))]));
        assert_eq!(occurrences.len(), 1);
    }

    #[test]
    fn nested_reference_resolves_through_the_dollar_separated_name() {
        let source = "package demo;\nclass Use { Outer.Inner x; }\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("demo/Outer$Inner", target(5, 1, 6))]));
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].target, target(5, 1, 6));
    }

    #[test]
    fn unresolved_reference_yields_no_occurrence() {
        let occurrences = definition_occurrences(
            "package demo;\nclass Use { Missing m; }\n",
            &ClassTargets::new(),
        );
        assert_eq!(occurrences, vec![]);
    }

    #[test]
    fn unresolved_explicit_import_does_not_fall_back_to_same_package() {
        let source = "package demo;\nimport unrelated.Greeter;\nclass Use { Greeter value; }\n";
        let occurrences =
            definition_occurrences(source, &classes(&[("demo/Greeter", target(9, 0, 7))]));
        assert!(occurrences.is_empty());
    }

    #[test]
    fn wildcard_import_ambiguity_does_not_pick_a_real_class() {
        let source =
            "package demo;\nimport first.*;\nimport second.*;\nclass Use { Greeter value; }\n";
        let occurrences = definition_occurrences(
            source,
            &classes(&[
                ("first/Greeter", target(2, 0, 7)),
                ("second/Greeter", target(3, 0, 7)),
            ]),
        );
        assert!(occurrences.is_empty());
    }

    #[test]
    fn type_parameters_and_body_identifiers_do_not_leak_class_targets() {
        let source = "package demo;\nclass Use<T> { T value; void f() { int Greeter = 1; consume(Greeter); } }\n";
        let occurrences = definition_occurrences(
            source,
            &classes(&[
                ("demo/T", target(2, 0, 1)),
                ("demo/Greeter", target(3, 0, 7)),
            ]),
        );
        assert!(occurrences.is_empty());
    }

    #[test]
    fn java_declarations_become_targets_at_their_name_spans() {
        let source = "package demo;\npublic class Widget implements java.io.Serializable {}\n";
        let declared = declared_classes(source, 4);
        let widget = declared.get("demo/Widget").copied().expect("demo/Widget");
        assert_eq!(widget.file, 4);
        assert_eq!(
            &source[widget.span.lo as usize..widget.span.hi as usize],
            "Widget"
        );
    }

    #[test]
    fn private_nested_declarations_resolve_inside_their_java_file() {
        let source = "package demo; class Outer { private class Secret {} Secret value; }";
        let classes = declared_classes(source, 4);
        let occurrences = definition_occurrences(source, &classes);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].target.file, 4);
        assert_eq!(
            &source[occurrences[0].target.span.lo as usize..occurrences[0].target.span.hi as usize],
            "Secret"
        );
    }
}
