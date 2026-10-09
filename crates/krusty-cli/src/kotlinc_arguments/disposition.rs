//! What krusty does with each kotlinc argument. Every argument of every supported release has
//! exactly one disposition (`every_kotlinc_argument_has_one_disposition`), so a new release's
//! arguments cannot reach users unclassified, and [`IGNORED`] is the complete list of kotlinc
//! arguments krusty does not implement yet.

/// krusty's handling of one kotlinc argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    /// `cli` reads the value and acts on it.
    Applied,
    /// Accepted and dropped with a note naming it. Each entry is a conformance gap: it moves to
    /// `Applied` (or a refusal) once krusty models it, with a test comparing kotlinc's output.
    Ignored,
}

/// The arguments `cli` applies, by canonical name.
pub const APPLIED: &[&str] = &[
    "-P",
    "-X",
    "-XXLanguage",
    "-Xcompiler-plugin",
    "-Xconsistent-data-class-copy-visibility",
    "-Xcontext-parameters",
    "-Xexplicit-api",
    "-Xexplicit-backing-fields",
    "-Xfriend-paths",
    "-Xjvm-default",
    "-Xlambdas",
    "-Xmetadata-version",
    "-Xmulti-dollar-interpolation",
    "-Xname-based-destructuring",
    "-Xnested-type-aliases",
    "-Xno-call-assertions",
    "-Xno-param-assertions",
    "-Xplugin",
    "-Xsam-conversions",
    "-Xsuppress-version-warnings",
    "-Xwarning-level",
    "-api-version",
    "-classpath",
    "-d",
    "-help",
    "-java-parameters",
    "-jdk-home",
    "-jvm-default",
    "-jvm-target",
    "-language-version",
    "-module-name",
    "-no-jdk",
    "-no-reflect",
    "-no-stdlib",
    "-opt-in",
    "-version",
];

/// kotlinc arguments krusty accepts without acting on them, by canonical name.
pub const IGNORED: &[&str] = &[
    "-Werror",
    "-Wextra",
    "-XXdebug-level-compiler-checks",
    "-XXdump-model",
    "-XXexplicit-return-types",
    "-XXlenient-mode",
    "-Xabi-stability",
    "-Xadd-modules",
    "-Xallow-any-scripts-in-source-roots",
    "-Xallow-condition-implies-returns-contracts",
    "-Xallow-contracts-on-more-functions",
    "-Xallow-holdsin-contract",
    "-Xallow-kotlin-package",
    "-Xallow-no-source-files",
    "-Xallow-reified-type-in-catch",
    "-Xallow-returns-result-of",
    "-Xallow-unstable-dependencies",
    "-Xannotation-default-target",
    "-Xannotation-target-all",
    "-Xannotations-in-metadata",
    "-Xassertions",
    "-Xbackend-threads",
    "-Xbuild-file",
    "-Xcheck-phase-conditions",
    "-Xcollection-literals",
    "-Xcommon-fragments-metadata-destination",
    "-Xcommon-sources",
    "-Xcompanion-blocks-and-extensions",
    "-Xcompiler-plugin-order",
    "-Xcontext-receivers",
    "-Xcontext-sensitive-resolution",
    "-Xdata-flow-based-exhaustiveness",
    "-Xdebug",
    "-Xdefault-script-extension",
    "-Xdetailed-perf",
    "-Xdirect-java-actualization",
    "-Xdisable-default-scripting-plugin",
    "-Xdisable-ir-checkers",
    "-Xdisable-phases",
    "-Xdisable-standard-script",
    "-Xdont-sort-source-files",
    "-Xdont-warn-on-error-suppression",
    "-Xdump-directory",
    "-Xdump-fqname",
    "-Xdump-perf",
    "-Xeager-lambda-analysis",
    "-Xemit-jvm-type-annotations",
    "-Xenable-additional-ir-checkers",
    "-Xenable-incremental-compilation",
    "-Xenhance-type-parameter-types-to-def-not-null",
    "-Xenhanced-coroutines-debugging",
    "-Xescaping-functions",
    "-Xexpect-actual-classes",
    "-Xexplicit-context-arguments",
    "-Xfir-aggressive-pruning",
    "-Xfragment-dependency",
    "-Xfragment-friend-dependency",
    "-Xfragment-incremental-classpath",
    "-Xfragment-refines",
    "-Xfragment-sources",
    "-Xfragments",
    "-Xgenerate-strict-metadata-version",
    "-Xheader-mode",
    "-Xheader-mode-type",
    "-Xignore-const-optimization-errors",
    "-Xignored-annotations-for-bridges",
    "-Xindy-allow-annotated-lambdas",
    "-Xinline-classes",
    "-Xintellij-plugin-root",
    "-Xintrinsic-const-evaluation",
    "-Xir-do-not-clear-binding-context",
    "-Xjava-package-prefix",
    "-Xjava-source-roots",
    "-Xjdk-release",
    "-Xjspecify-annotations",
    "-Xjsr305",
    "-Xjvm-enable-preview",
    "-Xjvm-expose-boxed",
    "-Xklib",
    "-Xlink-via-signatures",
    "-Xlist-phases",
    "-Xlocal-type-aliases",
    "-Xmetadata-klib",
    "-Xmodule-path",
    "-Xmulti-platform",
    "-Xmultifile-parts-inherit",
    "-Xnew-inference",
    "-Xno-check-actual",
    "-Xno-inline",
    "-Xno-new-java-annotation-targets",
    "-Xno-optimize",
    "-Xno-receiver-assertions",
    "-Xno-reset-jar-timestamps",
    "-Xno-source-debug-extension",
    "-Xno-unified-null-checks",
    "-Xnon-local-break-continue",
    "-Xnullability-annotations",
    "-Xoutput-builtins-metadata",
    "-Xphases-to-dump",
    "-Xphases-to-dump-after",
    "-Xphases-to-dump-before",
    "-Xphases-to-validate",
    "-Xphases-to-validate-after",
    "-Xphases-to-validate-before",
    "-Xprint-configuration",
    "-Xprofile",
    "-Xprofile-phases",
    "-Xrender-internal-diagnostic-names",
    "-Xrepl",
    "-Xreport-all-warnings",
    "-Xreport-output-files",
    "-Xreport-perf",
    "-Xreturn-value-checker",
    "-Xsanitize-parentheses",
    "-Xscript-resolver-environment",
    "-Xseparate-kmp-compilation",
    "-Xskip-metadata-version-check",
    "-Xskip-prerelease-check",
    "-Xstdlib-compilation",
    "-Xstring-concat",
    "-Xsupport-compatqual-checker-framework-annotations",
    "-Xsuppress-api-version-greater-than-language-version-error",
    "-Xsuppress-deprecated-jvm-target-warning",
    "-Xsuppress-missing-builtins-error",
    "-Xsuppress-warning",
    "-Xtype-enhancement-improvements-strict-mode",
    "-Xunrestricted-builder-inference",
    "-Xuse-14-inline-classes-mangling-scheme",
    "-Xuse-fast-jar-file-system",
    "-Xuse-fir-experimental-checkers",
    "-Xuse-fir-ic",
    "-Xuse-fir-lt",
    "-Xuse-inline-scopes-numbers",
    "-Xuse-k2",
    "-Xuse-old-class-files-reading",
    "-Xuse-type-table",
    "-Xvalidate-bytecode",
    "-Xvalue-classes",
    "-Xverbose-phases",
    "-Xverify-ir",
    "-Xverify-ir-nested-offsets",
    "-Xverify-ir-visibility",
    "-Xwhen-expressions",
    "-Xwhen-guards",
    "-expression",
    "-include-runtime",
    "-kotlin-home",
    "-nowarn",
    "-progressive",
    "-script",
    "-script-templates",
    "-verbose",
];

/// The disposition of a current kotlinc argument. A removed one has none: kotlinc itself only warns
/// that it has no effect.
pub fn of(name: &str) -> Option<Disposition> {
    if APPLIED.contains(&name) {
        Some(Disposition::Applied)
    } else if IGNORED.contains(&name) {
        Some(Disposition::Ignored)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlinc_arguments::catalog::{release_versions, Catalog, Origin};

    #[test]
    fn every_kotlinc_argument_has_one_disposition() {
        let mut known = std::collections::BTreeSet::new();
        for version in release_versions() {
            let catalog = Catalog::for_version(version).unwrap();
            for argument in catalog.arguments() {
                if argument.origin == Origin::Removed {
                    continue;
                }
                known.insert(argument.name.as_str());
                assert!(
                    of(&argument.name).is_some(),
                    "kotlinc {version} argument {} has no krusty disposition",
                    argument.name
                );
            }
        }
        for name in APPLIED.iter().chain(IGNORED) {
            assert!(known.contains(name), "{name} is not a kotlinc argument");
            assert!(
                !(APPLIED.contains(name) && IGNORED.contains(name)),
                "{name} has two dispositions"
            );
        }
    }
}
