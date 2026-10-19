//! `-Xfragments` and `-Xfragment-sources`: the multiplatform source-set structure a build tool
//! passes with the sources. krusty compiles one fragment, which holds the whole module, so its
//! output is the plain compilation's; kotlinc's checks that every source belongs to a declared
//! fragment are reproduced in its words.

/// The fragment arguments, in command-line order (each is a repeatable, comma-separated list).
#[derive(Default)]
pub(super) struct FragmentArguments {
    pub(super) fragments: Vec<String>,
    pub(super) fragment_sources: Vec<String>,
}

/// What the fragment arguments say about `free` (the free arguments as written).
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct FragmentCheck {
    /// kotlinc's configuration errors, in its order: each malformed, undeclared or doubly listed
    /// `-Xfragment-sources` value, then each free argument that no fragment lists.
    pub(super) configuration_errors: Vec<String>,
    /// A well-formed structure krusty does not compile: more than one fragment.
    pub(super) unsupported: Option<String>,
}

pub(super) fn check(arguments: &FragmentArguments, free: &[String]) -> FragmentCheck {
    // Without `-Xfragments` kotlinc ignores `-Xfragment-sources`.
    if arguments.fragments.is_empty() {
        return FragmentCheck::default();
    }
    // A fragment is its exact name, the empty name included: kotlinc accepts a name declared
    // twice, in one list or in repeated arguments, as one fragment.
    let fragments = distinct(&arguments.fragments);
    let mut configuration_errors = Vec::new();
    // Each listed source and the fragment it belongs to.
    let mut listed: Vec<(&str, &str)> = Vec::new();
    // kotlinc validates each distinct value and free spelling once, in first-seen order.
    for value in distinct(&arguments.fragment_sources) {
        match value.split_once(':') {
            None => configuration_errors.push(format!(
                "incorrect syntax for -Xfragment-sources argument. `<module name>:<source file>` \
                 expected but got `{value}`"
            )),
            Some((fragment, source)) if !fragments.contains(&fragment) => configuration_errors
                .push(format!(
                    "passed {value}, but fragment `{fragment}` of source file {source} is not \
                     specified in -Xfragments"
                )),
            Some((fragment, source)) => match listed.iter().find(|(seen, _)| *seen == source) {
                Some((_, owner)) => configuration_errors.push(format!(
                    "file '{source}' can be a part of only one module, but is listed as a source \
                     for both `{owner}` and `{fragment}`, please check you -Xfragment-sources \
                     options."
                )),
                None => listed.push((source, fragment)),
            },
        }
    }
    // kotlinc matches a free argument by its exact spelling, a directory included; a Java source
    // belongs to no fragment.
    for source in distinct(free) {
        if !source.ends_with(".java") && !listed.iter().any(|(seen, _)| *seen == source) {
            configuration_errors.push(format!("source '{source}' does not belong to any module"));
        }
    }
    // kotlinc reports a malformed structure whatever its shape; only a well-formed one with more
    // than one fragment reaches what krusty does not compile.
    let unsupported = (configuration_errors.is_empty() && fragments.len() > 1).then(|| {
        format!(
            "krusty compiles a single fragment, but -Xfragments declares {}: {}",
            fragments.len(),
            fragments.join(",")
        )
    });
    FragmentCheck {
        configuration_errors,
        unsupported,
    }
}

/// `values` without repeats, in first-seen order.
fn distinct(values: &[String]) -> Vec<&str> {
    let mut seen: Vec<&str> = Vec::new();
    for value in values {
        if !seen.contains(&value.as_str()) {
            seen.push(value);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    fn checked(fragments: &[&str], fragment_sources: &[&str], free: &[&str]) -> FragmentCheck {
        let arguments = FragmentArguments {
            fragments: strings(fragments),
            fragment_sources: strings(fragment_sources),
        };
        check(&arguments, &strings(free))
    }

    #[test]
    fn one_fragment_listing_every_kotlin_source_compiles() {
        assert_eq!(
            checked(
                &["main"],
                &["main:A.kt", "main:B.kt"],
                &["A.kt", "B.kt", "J.java"]
            ),
            FragmentCheck::default()
        );
    }

    #[test]
    fn fragment_sources_without_fragments_are_ignored() {
        assert_eq!(
            checked(&[], &["main:A.kt"], &["A.kt", "B.kt"]),
            FragmentCheck::default()
        );
    }

    /// The four errors and their order, as kotlinc 2.4.20 reports them for
    /// `A.kt B.kt d -Xfragments=main -Xfragment-sources=other:A.kt,B.kt,main:./B.kt`.
    #[test]
    fn unlisted_sources_are_kotlincs_configuration_errors() {
        assert_eq!(
            checked(
                &["main"],
                &["other:A.kt", "B.kt", "main:./B.kt"],
                &["A.kt", "B.kt", "d"]
            ),
            FragmentCheck {
                configuration_errors: strings(&[
                    "passed other:A.kt, but fragment `other` of source file A.kt is not \
                     specified in -Xfragments",
                    "incorrect syntax for -Xfragment-sources argument. `<module name>:<source \
                     file>` expected but got `B.kt`",
                    "source 'A.kt' does not belong to any module",
                    "source 'B.kt' does not belong to any module",
                    "source 'd' does not belong to any module",
                ]),
                unsupported: None,
            }
        );
    }

    /// kotlinc 2.4.20 compiles `-Xfragments=main,main` like `-Xfragments=main`.
    #[test]
    fn a_fragment_declared_twice_is_one_fragment() {
        assert_eq!(
            checked(&["main", "main"], &["main:A.kt"], &["A.kt"]),
            FragmentCheck::default()
        );
    }

    /// kotlinc reports each repeated value or free spelling once.
    #[test]
    fn repeated_values_and_spellings_are_reported_once() {
        assert_eq!(
            checked(
                &["main"],
                &["bad", "bad", "other:x", "other:x", "", ""],
                &["A.kt", "A.kt"]
            ),
            FragmentCheck {
                configuration_errors: strings(&[
                    "incorrect syntax for -Xfragment-sources argument. `<module name>:<source \
                     file>` expected but got `bad`",
                    "passed other:x, but fragment `other` of source file x is not specified in \
                     -Xfragments",
                    "incorrect syntax for -Xfragment-sources argument. `<module name>:<source \
                     file>` expected but got ``",
                    "source 'A.kt' does not belong to any module",
                ]),
                unsupported: None,
            }
        );
    }

    /// The empty name is a fragment like any other: `-Xfragments= -Xfragment-sources=:A.kt`
    /// compiles under kotlinc.
    #[test]
    fn the_empty_fragment_name_is_a_fragment() {
        assert_eq!(
            checked(&[""], &[":A.kt"], &["A.kt"]),
            FragmentCheck::default()
        );
    }

    #[test]
    fn more_than_one_distinct_fragment_is_refused_in_first_seen_order() {
        assert_eq!(
            checked(
                &["jvm", "common", "jvm"],
                &["common:A.kt", "jvm:B.kt"],
                &["A.kt", "B.kt"]
            )
            .unsupported
            .as_deref(),
            Some("krusty compiles a single fragment, but -Xfragments declares 2: jvm,common")
        );
    }

    /// kotlinc's own CLI cases (`compiler/testData/cli/jvm/hmpp`): with several fragments a
    /// malformed structure is still kotlinc's configuration error, not krusty's refusal.
    #[test]
    fn several_fragments_report_kotlincs_configuration_errors_first() {
        assert_eq!(
            checked(&["a", "b", "c"], &["a;a.kt;"], &["a.kt", "b.kt"]),
            FragmentCheck {
                configuration_errors: strings(&[
                    "incorrect syntax for -Xfragment-sources argument. `<module name>:<source \
                     file>` expected but got `a;a.kt;`",
                    "source 'a.kt' does not belong to any module",
                    "source 'b.kt' does not belong to any module",
                ]),
                unsupported: None,
            }
        );
        assert_eq!(
            checked(&["a", "b"], &[], &["a.kt"]).configuration_errors,
            strings(&["source 'a.kt' does not belong to any module"])
        );
    }

    #[test]
    fn a_source_listed_for_two_fragments_is_a_configuration_error() {
        assert_eq!(
            checked(
                &["a", "b"],
                &["a:a.kt", "b:a.kt", "b:b.kt"],
                &["a.kt", "b.kt"]
            ),
            FragmentCheck {
                configuration_errors: strings(&[
                    "file 'a.kt' can be a part of only one module, but is listed as a source for \
                     both `a` and `b`, please check you -Xfragment-sources options."
                ]),
                unsupported: None,
            }
        );
    }

    #[test]
    fn more_than_one_fragment_is_refused() {
        assert_eq!(
            checked(
                &["common", "jvm"],
                &["common:A.kt", "jvm:B.kt"],
                &["A.kt", "B.kt"]
            ),
            FragmentCheck {
                configuration_errors: Vec::new(),
                unsupported: Some(
                    "krusty compiles a single fragment, but -Xfragments declares 2: common,jvm"
                        .to_string()
                ),
            }
        );
    }
}
