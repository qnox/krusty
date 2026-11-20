//! Test-fixture `// LANGUAGE:` directives.

/// `// LANGUAGE: +X -Y` directives → the `-XXLanguage:` flags the reference test runner passes to
/// kotlinc, preceded by the JVM property that admits a test-only feature when one is named, as the
/// runner's own compiler runs under it. Each feature is spelled as the pinned reference release
/// knows it (`+CompanionBlocks` is `+CompanionBlocksAndExtensions` before 2.5). Krusty reads the
/// directives from the source itself.
pub fn kotlinc_args(src: &str) -> Vec<String> {
    let table = krusty::features::FeatureTable::for_version(krusty::kotlin_version::target())
        .expect("the pinned reference release has a feature table");
    let flags = src
        .lines()
        .filter_map(|line| line.trim().strip_prefix("// LANGUAGE:"))
        .flat_map(str::split_whitespace)
        .map(|token| table.language_argument(token.trim_end_matches(',')))
        .collect::<Vec<_>>();
    let mut arguments =
        krusty::conformance::reference_jvm_properties(flags.iter().map(String::as_str));
    arguments.extend(flags);
    arguments
}
