//! `-Xfragments`, `-Xfragment-sources` and `-Xfragment-refines`: the multiplatform source-set
//! structure a build tool passes with the sources. kotlinc compiles each fragment in its own
//! session and the last fragment of the refinement order is the platform leaf; krusty folds every
//! other fragment into the leaf's compilation as common sources, which is the leaf's own output.
//! Each source keeps its fragment, so a common source still sees only the fragments it refines.
//! kotlinc's checks of the structure are reproduced in its words.

use krusty::source::SourceFragment;

/// The fragment arguments, in command-line order (each is a repeatable, comma-separated list).
#[derive(Default)]
pub(super) struct FragmentArguments {
    pub(super) fragments: Vec<String>,
    pub(super) fragment_sources: Vec<String>,
    pub(super) fragment_refines: Vec<String>,
}

/// What the fragment arguments say about `free` (the free arguments as written).
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct FragmentCheck {
    /// kotlinc's configuration errors, in its order: each malformed, undeclared or doubly listed
    /// `-Xfragment-sources` value, each free argument that no fragment lists, then each problem
    /// of the refinement graph. Each message is reported once.
    pub(super) configuration_errors: Vec<String>,
    /// A well-formed structure krusty does not compile: a fragment the leaf does not refine.
    pub(super) unsupported: Option<String>,
    /// Each free argument a fragment lists, with that fragment. A fragment other than the leaf
    /// lists common sources.
    pub(super) sources: Vec<FragmentSource>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct FragmentSource {
    pub(super) source: String,
    pub(super) fragment: SourceFragment,
    pub(super) common: bool,
}

pub(super) fn check(arguments: &FragmentArguments, free: &[String]) -> FragmentCheck {
    let mut errors = Errors::default();
    // Without `-Xfragments` kotlinc ignores `-Xfragment-sources`.
    if arguments.fragments.is_empty() {
        if !arguments.fragment_refines.is_empty() {
            errors.push("-Xfragment-refines flag can not be used without -Xfragments".to_string());
        }
        return errors.into_check();
    }
    // A fragment is its exact name, the empty name included: kotlinc accepts a name declared
    // twice, in one list or in repeated arguments, as one fragment.
    let fragments = distinct(&arguments.fragments);
    // Each listed source and the fragment it belongs to.
    let mut listed: Vec<(&str, &str)> = Vec::new();
    // kotlinc validates each distinct value and free spelling once, in first-seen order. A
    // malformed or undeclared value still lets kotlinc check the refinement graph; a source in
    // two fragments or in none stops it.
    let mut membership_error = false;
    for value in distinct(&arguments.fragment_sources) {
        match value.split_once(':') {
            None => errors.push(format!(
                "incorrect syntax for -Xfragment-sources argument. `<module name>:<source file>` \
                 expected but got `{value}`"
            )),
            Some((fragment, source)) if !fragments.contains(&fragment) => errors.push(format!(
                "passed {value}, but fragment `{fragment}` of source file {source} is not \
                 specified in -Xfragments"
            )),
            Some((fragment, source)) => match listed.iter().find(|(seen, _)| *seen == source) {
                Some((_, owner)) => {
                    membership_error = true;
                    errors.push(format!(
                        "file '{source}' can be a part of only one module, but is listed as a \
                         source for both `{owner}` and `{fragment}`, please check you \
                         -Xfragment-sources options."
                    ))
                }
                None => listed.push((source, fragment)),
            },
        }
    }
    // kotlinc matches a free argument by its exact spelling, a directory included; a Java source
    // belongs to no fragment.
    for source in distinct(free) {
        if !source.ends_with(".java") && !listed.iter().any(|(seen, _)| *seen == source) {
            membership_error = true;
            errors.push(format!("source '{source}' does not belong to any module"));
        }
    }
    if membership_error {
        return errors.into_check();
    }
    if fragments.len() == 1 {
        if !arguments.fragment_refines.is_empty() {
            errors.push(
                "-Xfragment-refines flag is specified but there is only one module declared"
                    .to_string(),
            );
        }
        return errors.into_check();
    }
    let refines = refinement_edges(&fragments, &arguments.fragment_refines, &mut errors);
    let order = refinement_order(&fragments, &refines);
    for (index, fragment) in order.iter().enumerate() {
        if refined(&refines, fragment).any(|parent| !order[..index].contains(&parent)) {
            errors.push(format!(
                "there is a cycle in dependencies of module `{fragment}`"
            ));
        }
    }
    if !errors.0.is_empty() {
        return errors.into_check();
    }
    // kotlinc compiles the last fragment as the platform leaf, seeing only the fragments it
    // refines; folding them into one compilation is that output only when the leaf refines every
    // other fragment.
    let (leaf, common) = order.split_last().expect("more than one fragment");
    let reachable = refined_closure(&refines, leaf);
    let unrefined = common
        .iter()
        .filter(|fragment| !reachable.contains(fragment))
        .copied()
        .collect::<Vec<_>>();
    if !unrefined.is_empty() {
        return FragmentCheck {
            unsupported: Some(format!(
                "krusty compiles fragments the leaf refines, but `{leaf}` does not refine {}",
                unrefined
                    .iter()
                    .map(|fragment| format!("`{fragment}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            ..FragmentCheck::default()
        };
    }
    if order.len() > SourceFragment::MAX {
        return FragmentCheck {
            unsupported: Some(format!(
                "krusty compiles at most {} fragments, but {} are declared",
                SourceFragment::MAX,
                order.len()
            )),
            ..FragmentCheck::default()
        };
    }
    let index = |fragment: &str| {
        order
            .iter()
            .position(|ordered| *ordered == fragment)
            .expect("every fragment is ordered")
    };
    let sources = free
        .iter()
        .filter_map(|source| {
            let (_, owner) = listed.iter().find(|(seen, _)| seen == source)?;
            Some(FragmentSource {
                source: source.clone(),
                fragment: SourceFragment::new(
                    index(owner),
                    refined_closure(&refines, owner).into_iter().map(index),
                ),
                common: owner != leaf,
            })
        })
        .collect();
    FragmentCheck {
        sources,
        ..FragmentCheck::default()
    }
}

/// kotlinc's configuration errors; its message collector reports a repeated message once.
#[derive(Default)]
struct Errors(Vec<String>);

impl Errors {
    fn push(&mut self, message: String) {
        if !self.0.contains(&message) {
            self.0.push(message);
        }
    }

    fn into_check(self) -> FragmentCheck {
        FragmentCheck {
            configuration_errors: self.0,
            ..FragmentCheck::default()
        }
    }
}

/// The `from:on` edges of `-Xfragment-refines`, in command-line order, between declared
/// fragments. kotlinc reports a value that is not two names (printing its argument array's
/// identity, which krusty spells as the value) and each undeclared name, then drops that edge.
fn refinement_edges<'a>(
    fragments: &[&'a str],
    values: &'a [String],
    errors: &mut Errors,
) -> Vec<(&'a str, &'a str)> {
    let mut edges = Vec::new();
    for value in values {
        let names = value.split(':').collect::<Vec<_>>();
        let [from, on] = names[..] else {
            errors.push(format!(
                "incorrect syntax for -Xfragment-refines argument. Expected \
                 <fromModuleName>:<onModuleName> but got `{value}`"
            ));
            continue;
        };
        let declared = |name: &'a str, errors: &mut Errors| {
            let found = fragments.iter().copied().find(|fragment| *fragment == name);
            if found.is_none() {
                errors.push(format!(
                    "`-Xfragment-refines={value}` Fragment `{name}` not found in -Xfragments \
                     arguments"
                ));
            }
            found
        };
        if let (Some(from), Some(on)) = (declared(from, errors), declared(on, errors)) {
            edges.push((from, on));
        }
    }
    edges
}

/// The fragments `fragment` directly refines, in command-line order.
fn refined<'s, 'a: 's>(
    refines: &'s [(&'a str, &'a str)],
    fragment: &'s str,
) -> impl Iterator<Item = &'a str> + 's {
    refines
        .iter()
        .filter(move |(from, _)| *from == fragment)
        .map(|(_, on)| *on)
}

/// kotlinc's compilation order: the depth-first post-order over the refinement edges, starting
/// from each fragment in declaration order, so a refined fragment precedes its refiners unless
/// they form a cycle.
fn refinement_order<'a>(fragments: &[&'a str], refines: &[(&'a str, &'a str)]) -> Vec<&'a str> {
    fn visit<'a>(
        fragment: &'a str,
        refines: &[(&'a str, &'a str)],
        order: &mut Vec<&'a str>,
        visited: &mut Vec<&'a str>,
    ) {
        if visited.contains(&fragment) {
            return;
        }
        visited.push(fragment);
        for parent in refined(refines, fragment) {
            visit(parent, refines, order, visited);
        }
        order.push(fragment);
    }
    let (mut order, mut visited) = (Vec::new(), Vec::new());
    for fragment in fragments {
        visit(fragment, refines, &mut order, &mut visited);
    }
    order
}

/// Every fragment `fragment` refines, directly or transitively.
fn refined_closure<'a>(refines: &[(&'a str, &'a str)], fragment: &'a str) -> Vec<&'a str> {
    let mut closure: Vec<&str> = Vec::new();
    let mut pending = vec![fragment];
    while let Some(next) = pending.pop() {
        for parent in refined(refines, next) {
            if !closure.contains(&parent) {
                closure.push(parent);
                pending.push(parent);
            }
        }
    }
    closure
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

    fn fragment_source(source: &str, fragment: SourceFragment, common: bool) -> FragmentSource {
        FragmentSource {
            source: source.to_string(),
            fragment,
            common,
        }
    }

    fn checked(fragments: &[&str], fragment_sources: &[&str], free: &[&str]) -> FragmentCheck {
        refined(fragments, fragment_sources, &[], free)
    }

    fn refined(
        fragments: &[&str],
        fragment_sources: &[&str],
        fragment_refines: &[&str],
        free: &[&str],
    ) -> FragmentCheck {
        let arguments = FragmentArguments {
            fragments: strings(fragments),
            fragment_sources: strings(fragment_sources),
            fragment_refines: strings(fragment_refines),
        };
        check(&arguments, &strings(free))
    }

    fn errors(messages: &[&str]) -> FragmentCheck {
        FragmentCheck {
            configuration_errors: strings(messages),
            ..FragmentCheck::default()
        }
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
                sources: Vec::new(),
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
                sources: Vec::new(),
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

    /// Without refinement edges kotlinc compiles the last fragment as the leaf, which sees no
    /// other fragment; krusty refuses rather than fold sources the leaf cannot see.
    #[test]
    fn a_fragment_the_leaf_does_not_refine_is_refused() {
        assert_eq!(
            checked(
                &["jvm", "common", "jvm"],
                &["common:A.kt", "jvm:B.kt"],
                &["A.kt", "B.kt"]
            ),
            FragmentCheck {
                configuration_errors: Vec::new(),
                unsupported: Some(
                    "krusty compiles fragments the leaf refines, but `common` does not refine \
                     `jvm`"
                        .to_string()
                ),
                sources: Vec::new(),
            }
        );
    }

    /// kotlinc's `hmpp/successfulCompilation`: `c` refines `b`, which refines `a`.
    #[test]
    fn the_fragments_a_leaf_refines_are_common_sources() {
        assert_eq!(
            refined(
                &["c", "b", "a"],
                &["a:a.kt", "b:b.kt", "c:c.kt"],
                &["b:a", "c:b"],
                &["a.kt", "b.kt", "c.kt", "J.java"]
            ),
            FragmentCheck {
                sources: vec![
                    fragment_source("a.kt", SourceFragment::new(0, []), true),
                    fragment_source("b.kt", SourceFragment::new(1, [0]), true),
                    fragment_source("c.kt", SourceFragment::new(2, [0, 1]), false),
                ],
                ..FragmentCheck::default()
            }
        );
    }

    fn as_strs(values: &[String]) -> Vec<&str> {
        values.iter().map(String::as_str).collect()
    }

    #[test]
    fn more_fragments_than_krusty_distinguishes_are_refused() {
        let names = (0..=SourceFragment::MAX)
            .map(|index| format!("f{index}"))
            .collect::<Vec<_>>();
        let sources = names
            .iter()
            .map(|name| format!("{name}:{name}.kt"))
            .collect::<Vec<_>>();
        let refines = names
            .windows(2)
            .map(|pair| format!("{}:{}", pair[1], pair[0]))
            .collect::<Vec<_>>();
        let free = names
            .iter()
            .map(|name| format!("{name}.kt"))
            .collect::<Vec<_>>();
        assert_eq!(
            refined(
                &as_strs(&names),
                &as_strs(&sources),
                &as_strs(&refines),
                &as_strs(&free)
            ),
            FragmentCheck {
                unsupported: Some(
                    "krusty compiles at most 64 fragments, but 65 are declared".to_string()
                ),
                ..FragmentCheck::default()
            }
        );
    }

    /// The leaf is the last fragment of the refinement order, not of the declaration order.
    #[test]
    fn the_leaf_is_the_last_fragment_in_refinement_order() {
        assert_eq!(
            refined(
                &["jvm", "common"],
                &["common:A.kt", "jvm:B.kt"],
                &["jvm:common"],
                &["A.kt", "B.kt"]
            ),
            FragmentCheck {
                sources: vec![
                    fragment_source("A.kt", SourceFragment::new(0, []), true),
                    fragment_source("B.kt", SourceFragment::new(1, [0]), false),
                ],
                ..FragmentCheck::default()
            }
        );
    }

    #[test]
    fn refines_without_fragments_is_a_configuration_error() {
        assert_eq!(
            refined(&[], &[], &["a:b"], &["a.kt"]),
            errors(&["-Xfragment-refines flag can not be used without -Xfragments"])
        );
    }

    #[test]
    fn refines_with_one_fragment_is_a_configuration_error() {
        assert_eq!(
            refined(&["a", "a"], &["a:a.kt"], &["b:a"], &["a.kt"]),
            errors(&["-Xfragment-refines flag is specified but there is only one module declared"])
        );
    }

    /// Each undeclared name of an edge is reported, a repeated message once (kotlinc 2.4.20 for
    /// `-Xfragment-refines=c:a,c:a,b:d`).
    #[test]
    fn an_undeclared_refined_fragment_is_a_configuration_error() {
        assert_eq!(
            refined(
                &["a", "b"],
                &["a:a.kt", "b:b.kt"],
                &["c:a", "c:a", "b:d", "e:f"],
                &["a.kt", "b.kt"]
            ),
            errors(&[
                "`-Xfragment-refines=c:a` Fragment `c` not found in -Xfragments arguments",
                "`-Xfragment-refines=b:d` Fragment `d` not found in -Xfragments arguments",
                "`-Xfragment-refines=e:f` Fragment `e` not found in -Xfragments arguments",
                "`-Xfragment-refines=e:f` Fragment `f` not found in -Xfragments arguments",
            ])
        );
    }

    /// kotlinc prints the identity of its whole argument array; krusty prints the value.
    #[test]
    fn an_edge_that_is_not_two_names_is_a_configuration_error() {
        assert_eq!(
            refined(
                &["a", "b"],
                &["a:a.kt", "b:b.kt"],
                &["b:a:c", "b"],
                &["a.kt", "b.kt"]
            ),
            errors(&[
                "incorrect syntax for -Xfragment-refines argument. Expected \
                 <fromModuleName>:<onModuleName> but got `b:a:c`",
                "incorrect syntax for -Xfragment-refines argument. Expected \
                 <fromModuleName>:<onModuleName> but got `b`",
            ])
        );
    }

    /// kotlinc's `hmpp/cycleInDependencies` and `hmpp/commaInFragmentDependency`: a malformed
    /// source value does not stop the refinement check, which names the first fragment of the
    /// depth-first order whose refined fragment does not precede it.
    #[test]
    fn a_refinement_cycle_is_a_configuration_error() {
        let sources = ["a:a.kt", "b:b.kt", "c:c.kt"];
        let free = ["a.kt", "b.kt", "c.kt"];
        assert_eq!(
            refined(&["a", "b", "c"], &sources, &["b:a", "c:b", "a:c"], &free),
            errors(&["there is a cycle in dependencies of module `b`"])
        );
        assert_eq!(
            refined(
                &["a", "b", "c"],
                &["a:a.kt", " src/a.kt", "b:b.kt", "c:c.kt"],
                &["b:a", "c:b", "a:c"],
                &free
            ),
            errors(&[
                "incorrect syntax for -Xfragment-sources argument. `<module name>:<source \
                 file>` expected but got ` src/a.kt`",
                "there is a cycle in dependencies of module `b`",
            ])
        );
        assert_eq!(
            refined(
                &["a", "b"],
                &["a:a.kt", "b:b.kt"],
                &["b:b"],
                &["a.kt", "b.kt"]
            ),
            errors(&["there is a cycle in dependencies of module `b`"])
        );
    }

    /// A source in no fragment stops kotlinc before the refinement graph.
    #[test]
    fn a_membership_error_stops_before_the_refinement_check() {
        assert_eq!(
            refined(&["a", "b"], &["a:a.kt"], &["b:a", "x:a"], &["a.kt", "b.kt"]),
            errors(&["source 'b.kt' does not belong to any module"])
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
                sources: Vec::new(),
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
                sources: Vec::new(),
            }
        );
    }
}
