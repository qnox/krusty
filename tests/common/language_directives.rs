//! Test-fixture `// LANGUAGE:` directives.

/// `// LANGUAGE: +X -Y` directives → the `-XXLanguage:` flags the reference test runner passes to
/// kotlinc. Krusty reads the directives from the source itself.
pub fn kotlinc_args(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|line| line.trim().strip_prefix("// LANGUAGE:"))
        .flat_map(str::split_whitespace)
        .map(|token| format!("-XXLanguage:{token}"))
        .collect()
}
