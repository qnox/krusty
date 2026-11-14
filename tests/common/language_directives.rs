//! Test-fixture `// LANGUAGE:` directives.

/// `// LANGUAGE: +X -Y` directives → the `-XXLanguage:` flags the reference test runner passes to
/// kotlinc, preceded by the JVM property that admits a test-only feature when one is named, as the
/// runner's own compiler runs under it. Krusty reads the directives from the source itself.
pub fn kotlinc_args(src: &str) -> Vec<String> {
    let flags = src
        .lines()
        .filter_map(|line| line.trim().strip_prefix("// LANGUAGE:"))
        .flat_map(str::split_whitespace)
        .map(|token| format!("-XXLanguage:{}", token.trim_end_matches(',')))
        .collect::<Vec<_>>();
    let mut arguments =
        krusty::conformance::reference_jvm_properties(flags.iter().map(String::as_str));
    arguments.extend(flags);
    arguments
}
