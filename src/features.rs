//! Language-feature flags — krusty's model of kotlinc's `-XXLanguage:`/`-X` toggles and the test
//! infrastructure's `// LANGUAGE:` directive. Stable features for krusty's Kotlin language level are
//! enabled by default; directives and command-line flags apply ordered overrides.

use std::collections::HashSet;

/// The set of enabled language features (by their kotlinc `LanguageFeature` name, e.g.
/// `NameBasedDestructuring`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LangFeatures {
    enabled: HashSet<String>,
}

impl Default for LangFeatures {
    fn default() -> Self {
        let mut enabled = HashSet::new();
        for &(name, since) in OBSERVED_FEATURES {
            if let Some(since) = since {
                if language_level_enables(since) {
                    enabled.insert(name.to_string());
                }
            }
        }
        Self { enabled }
    }
}

/// Krusty's language level is kotlinc 2.4.20's default. A feature whose `sinceVersion` is at
/// most this level is on; one with no `sinceVersion`, or a later one, stays opt-in.
const LANGUAGE_LEVEL: (u16, u16) = (2, 4);

/// Features the frontend observes, paired with kotlinc's `sinceVersion`. `None` means kotlinc
/// has not assigned one, so the feature stays off until a flag or directive enables it.
const OBSERVED_FEATURES: &[(&str, Option<(u16, u16)>)] = &[
    (
        "AllowAccessToProtectedFieldFromSuperCompanion",
        Some((2, 1)),
    ),
    ("BareArrayClassLiteral", Some((1, 4))),
    ("ContextParameters", Some((2, 4))),
    ("ContextReceivers", None),
    ("ContextSensitiveResolutionUsingExpectedType", None),
    ("DataClassCopyRespectsConstructorVisibility", None),
    ("EagerLambdaAnalysis", None),
    ("EnableNameBasedDestructuringShortForm", None),
    ("EnumEntries", Some((1, 9))),
    ("ExplicitBackingFields", Some((2, 4))),
    ("ExplicitContextArguments", Some((2, 5))),
    ("ImplicitSignedToUnsignedIntegerConversion", None),
    ("MultiDollarInterpolation", Some((2, 2))),
    ("MultiPlatformProjects", None),
    ("NameBasedDestructuring", Some((2, 5))),
    ("PrioritizedEnumEntries", Some((2, 1))),
    ("UnitConversionsOnArbitraryExpressions", None),
    ("WhenGuards", Some((2, 2))),
];

fn language_level_enables(since: (u16, u16)) -> bool {
    since.0 < LANGUAGE_LEVEL.0 || (since.0 == LANGUAGE_LEVEL.0 && since.1 <= LANGUAGE_LEVEL.1)
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

    /// Enable every feature enabled in `other`.
    pub fn extend(&mut self, other: &Self) {
        self.enabled.extend(other.enabled.iter().cloned());
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.enabled.iter().map(String::as_str)
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
    pub fn apply_source_directives(&mut self, src: &str) {
        for line in src.lines() {
            let l = line.trim_start();
            if let Some(rest) = l.strip_prefix("// LANGUAGE:") {
                self.apply_directive(rest);
            }
            // `// ASSERTIONS_MODE: always-enable|always-disable` — kotlinc's `-Xassertions` mode for the
            // `assert(...)` intrinsic (modeled as pseudo-features so it flows like any other directive).
            if let Some(rest) = l.strip_prefix("// ASSERTIONS_MODE:") {
                match rest.trim() {
                    "always-enable" => self.enable("AssertionsAlwaysEnable"),
                    "always-disable" => self.enable("AssertionsAlwaysDisable"),
                    _ => {}
                }
            }
        }
    }

    /// Collect every `// LANGUAGE:` directive in a source file.
    pub fn from_source(src: &str) -> Self {
        let mut f = Self::default();
        f.apply_source_directives(src);
        f
    }

    /// Apply a single CLI argument, mirroring the reference compiler's flags. Returns `true` if the
    /// argument was a recognized language flag (the caller should then not treat it as a source file).
    /// Handles `-XXLanguage:+Foo,-Bar`, the `-Xname-based-destructuring[=mode]` alias, and each
    /// one-feature flag in `FEATURE_ALIASES`.
    pub fn apply_cli_arg(&mut self, arg: &str) -> bool {
        if let Some(rest) = arg.strip_prefix("-XXLanguage:") {
            self.apply_directive(rest);
            return true;
        }
        if let Some(rest) = arg.strip_prefix("-Xname-based-destructuring") {
            // `-Xname-based-destructuring[=only-syntax|name-mismatch|complete|disable]`.
            match rest {
                "=disable" => {
                    self.disable("NameBasedDestructuring");
                    self.disable("EnableNameBasedDestructuringShortForm");
                }
                "=complete" => {
                    self.enable("NameBasedDestructuring");
                    self.enable("EnableNameBasedDestructuringShortForm");
                }
                _ => {
                    self.enable("NameBasedDestructuring");
                    self.disable("EnableNameBasedDestructuringShortForm");
                }
            }
            return true;
        }
        if let Some(feature) = feature_alias(arg) {
            self.enable(feature);
            return true;
        }
        false
    }
}

/// Flags that enable exactly one language feature. A mode (`-Xname-based-destructuring=…`) or a
/// list (`-XXLanguage:`) is not an entry: those change more than one feature.
const FEATURE_ALIASES: &[(&str, &str)] = &[
    ("-Xcontext-parameters", "ContextParameters"),
    (
        "-Xconsistent-data-class-copy-visibility",
        "DataClassCopyRespectsConstructorVisibility",
    ),
    ("-Xexplicit-backing-fields", "ExplicitBackingFields"),
    ("-Xmulti-dollar-interpolation", "MultiDollarInterpolation"),
];

fn feature_alias(arg: &str) -> Option<&'static str> {
    FEATURE_ALIASES
        .iter()
        .find_map(|&(flag, feature)| (flag == arg).then_some(feature))
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
    fn source_directives_are_applied_over_project_features() {
        let mut features = LangFeatures::new();
        features.enable("ProjectFeature");
        features.apply_source_directives(
            "// LANGUAGE: -ProjectFeature +SourceFeature\nfun box() = \"OK\"\n",
        );
        assert!(!features.has("ProjectFeature"));
        assert!(features.has("SourceFeature"));
    }

    #[test]
    fn cli_xxlanguage_and_alias() {
        let mut f = LangFeatures::new();
        assert!(f.apply_cli_arg("-XXLanguage:+NameBasedDestructuring"));
        assert!(f.has("NameBasedDestructuring"));
        let mut g = LangFeatures::new();
        assert!(g.apply_cli_arg("-Xname-based-destructuring=complete"));
        assert!(g.has("NameBasedDestructuring"));
        assert!(g.has("EnableNameBasedDestructuringShortForm"));
        assert!(g.apply_cli_arg("-Xname-based-destructuring=disable"));
        assert!(!g.has("NameBasedDestructuring"));
        assert!(!g.has("EnableNameBasedDestructuringShortForm"));
        assert!(!g.apply_cli_arg("foo.kt"));
    }

    #[test]
    fn language_level_enables_features_stable_in_2_4() {
        let features = LangFeatures::new();
        for name in [
            "AllowAccessToProtectedFieldFromSuperCompanion",
            "BareArrayClassLiteral",
            "ContextParameters",
            "EnumEntries",
            "ExplicitBackingFields",
            "MultiDollarInterpolation",
            "PrioritizedEnumEntries",
            "WhenGuards",
        ] {
            assert!(features.has(name), "{name} is stable in 2.4");
        }
        for name in [
            "ContextReceivers",
            "DataClassCopyRespectsConstructorVisibility",
            "ExplicitContextArguments",
            "NameBasedDestructuring",
        ] {
            assert!(!features.has(name), "{name} stays opt-in");
        }
    }

    #[test]
    fn each_cli_feature_alias_enables_its_feature() {
        assert!(LangFeatures::new().has("ExplicitBackingFields"));
        assert!(!LangFeatures::new().has("DataClassCopyRespectsConstructorVisibility"));
        for &(flag, feature) in super::FEATURE_ALIASES {
            let mut features = LangFeatures::new();
            assert!(features.apply_cli_arg(flag), "{flag}");
            assert!(features.has(feature), "{flag} enables {feature}");
        }
    }

    #[test]
    fn stable_features_can_be_disabled_and_reenabled() {
        let mut features = LangFeatures::new();
        assert!(features.has("MultiDollarInterpolation"));
        features.apply_directive("-MultiDollarInterpolation");
        assert!(!features.has("MultiDollarInterpolation"));
        features.apply_directive("+MultiDollarInterpolation");
        assert!(features.has("MultiDollarInterpolation"));
    }
}
