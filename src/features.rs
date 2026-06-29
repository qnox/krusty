//! Language-feature flags — krusty's model of kotlinc's `-XXLanguage:`/`-X` toggles and the test
//! infrastructure's `// LANGUAGE:` directive. A DEFAULT compile enables none of these (matching a
//! default-flags `kotlinc`); a drop-in enables a feature only when its flag/directive is present, so
//! experimental syntax (e.g. name-based `[a, b]` destructuring) is rejected by default and accepted
//! only under the corresponding flag — exactly as the reference compiler behaves.

use std::collections::HashSet;

/// The set of enabled language features (by their kotlinc `LanguageFeature` name, e.g.
/// `NameBasedDestructuring`). Empty = the default language version's feature set.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct LangFeatures {
    enabled: HashSet<String>,
}

impl LangFeatures {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `name` (a kotlinc `LanguageFeature` identifier) is enabled.
    pub fn has(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn enable(&mut self, name: &str) {
        self.enabled.insert(name.to_string());
    }

    pub fn disable(&mut self, name: &str) {
        self.enabled.remove(name);
    }

    /// Apply the payload of a `// LANGUAGE:` directive / `-XXLanguage:` flag: whitespace- or
    /// comma-separated `+Feature` / `-Feature` tokens (`+` enables, `-` disables).
    pub fn apply_directive(&mut self, payload: &str) {
        for tok in payload.split([' ', ',', '\t']).filter(|s| !s.is_empty()) {
            if let Some(name) = tok.strip_prefix('+') {
                self.enable(name);
            } else if let Some(name) = tok.strip_prefix('-') {
                self.disable(name);
            }
        }
    }

    /// Collect every `// LANGUAGE:` directive in a source file. This is how the kotlinc test
    /// infrastructure (and thus our conformance harness) specifies the flags a test compiles under.
    pub fn from_source(src: &str) -> Self {
        let mut f = Self::default();
        for line in src.lines() {
            let l = line.trim_start();
            if let Some(rest) = l.strip_prefix("// LANGUAGE:") {
                f.apply_directive(rest);
            }
            // `// ASSERTIONS_MODE: always-enable|always-disable` — kotlinc's `-Xassertions` mode for the
            // `assert(...)` intrinsic (modeled as pseudo-features so it flows like any other directive).
            if let Some(rest) = l.strip_prefix("// ASSERTIONS_MODE:") {
                match rest.trim() {
                    "always-enable" => f.enable("AssertionsAlwaysEnable"),
                    "always-disable" => f.enable("AssertionsAlwaysDisable"),
                    _ => {}
                }
            }
        }
        f
    }

    /// Apply a single CLI argument, mirroring the reference compiler's flags. Returns `true` if the
    /// argument was a recognized language flag (the caller should then not treat it as a source file).
    /// Handles `-XXLanguage:+Foo,-Bar` and the `-Xname-based-destructuring[=mode]` alias.
    pub fn apply_cli_arg(&mut self, arg: &str) -> bool {
        if let Some(rest) = arg.strip_prefix("-XXLanguage:") {
            self.apply_directive(rest);
            return true;
        }
        if let Some(rest) = arg.strip_prefix("-Xname-based-destructuring") {
            // `-Xname-based-destructuring[=only-syntax|name-mismatch|complete|disable]`.
            if rest != "=disable" {
                self.enable("NameBasedDestructuring");
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_enables_and_disables() {
        let mut f = LangFeatures::new();
        f.apply_directive("+NameBasedDestructuring +Other");
        assert!(f.has("NameBasedDestructuring"));
        assert!(f.has("Other"));
        f.apply_directive("-Other");
        assert!(!f.has("Other"));
    }

    #[test]
    fn from_source_reads_language_lines() {
        let src = "// WITH_STDLIB\n// LANGUAGE: +NameBasedDestructuring\nfun box() = \"OK\"\n";
        let f = LangFeatures::from_source(src);
        assert!(f.has("NameBasedDestructuring"));
    }

    #[test]
    fn cli_xxlanguage_and_alias() {
        let mut f = LangFeatures::new();
        assert!(f.apply_cli_arg("-XXLanguage:+NameBasedDestructuring"));
        assert!(f.has("NameBasedDestructuring"));
        let mut g = LangFeatures::new();
        assert!(g.apply_cli_arg("-Xname-based-destructuring=complete"));
        assert!(g.has("NameBasedDestructuring"));
        assert!(!g.apply_cli_arg("foo.kt"));
    }
}
