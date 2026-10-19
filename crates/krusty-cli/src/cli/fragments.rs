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
    /// kotlinc's configuration errors, in its order: each malformed or undeclared
    /// `-Xfragment-sources` value, then each free argument that no fragment lists.
    pub(super) configuration_errors: Vec<String>,
    /// A structure krusty does not compile: more than one fragment.
    pub(super) unsupported: Option<String>,
}

pub(super) fn check(arguments: &FragmentArguments, free: &[String]) -> FragmentCheck {
    // Without `-Xfragments` kotlinc ignores `-Xfragment-sources`.
    if arguments.fragments.is_empty() {
        return FragmentCheck::default();
    }
    if arguments.fragments.len() > 1 {
        return FragmentCheck {
            configuration_errors: Vec::new(),
            unsupported: Some(format!(
                "krusty compiles a single fragment, but -Xfragments declares {}: {}",
                arguments.fragments.len(),
                arguments.fragments.join(",")
            )),
        };
    }
    let mut configuration_errors = Vec::new();
    let mut listed = Vec::new();
    for value in &arguments.fragment_sources {
        match value.split_once(':') {
            None => configuration_errors.push(format!(
                "incorrect syntax for -Xfragment-sources argument. `<module name>:<source file>` \
                 expected but got `{value}`"
            )),
            Some((fragment, source)) if !arguments.fragments.iter().any(|f| f == fragment) => {
                configuration_errors.push(format!(
                    "passed {value}, but fragment `{fragment}` of source file {source} is not \
                     specified in -Xfragments"
                ))
            }
            Some((_, source)) => listed.push(source),
        }
    }
    // kotlinc matches a free argument by its exact spelling, a directory included; a Java source
    // belongs to no fragment.
    for source in free {
        if !source.ends_with(".java") && !listed.contains(&source.as_str()) {
            configuration_errors.push(format!("source '{source}' does not belong to any module"));
        }
    }
    FragmentCheck {
        configuration_errors,
        unsupported: None,
    }
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
