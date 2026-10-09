//! Language-feature flags — krusty's model of kotlinc's `-XXLanguage:`/`-X` toggles and the test
//! infrastructure's `// LANGUAGE:` directive. The defaults for a language/API version pair come from
//! the reference release's own `LanguageFeature` table ([`FeatureTable`]); directives and
//! command-line flags apply ordered overrides on top of them.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::kotlin_version::KotlinVersion;
use crate::language_version::LanguageVersion;

mod language_versions;
mod table;
mod unsupported;

pub use language_versions::{vendored_language_versions, LanguageVersionPolicy, VersionStatus};
pub use table::{vendored_table, BehaviorAfterSinceVersion, FeatureTable, LanguageFeature};
pub use unsupported::FeatureGate;

/// One explicit language-feature state, as kotlinc's `specificFeatures` map holds it: the effect of
/// an `@Enables`/`@Disables` argument, a `-XXLanguage:` argument, or a `// LANGUAGE:` directive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureSetting {
    pub feature: String,
    pub enabled: bool,
}

/// The set of enabled language features (by their kotlinc `LanguageFeature` name, e.g.
/// `NameBasedDestructuring`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LangFeatures {
    /// The reference release whose feature table supplies the defaults.
    release: KotlinVersion,
    language_version: LanguageVersion,
    api_version: LanguageVersion,
    enabled: HashSet<String>,
    /// The explicitly set features and their states, in kotlinc's `specificFeatures` sense. These
    /// decide whether the output is pre-release ([`Self::is_pre_release`]).
    specific: BTreeMap<String, bool>,
    /// Opt-in requirement markers accepted for the whole module (`-opt-in=<fq name>`), spelled as
    /// kotlinc's `AnalysisFlags.optIn` holds them: dotted fully qualified names.
    opted_in: BTreeSet<String>,
}

impl Default for LangFeatures {
    fn default() -> Self {
        Self::for_language_version(LanguageVersion::default())
    }
}

impl LangFeatures {
    /// The default feature set for one public Kotlin source-language level.
    pub fn for_language_version(language_version: LanguageVersion) -> Self {
        Self::for_versions(language_version, language_version)
    }

    /// The default feature set for one language/API pair: every feature of the reference
    /// release's table whose `sinceVersion` and `sinceApiVersion` are both reached (kotlinc's
    /// `isEnabledByDefault`).
    pub fn for_versions(language_version: LanguageVersion, api_version: LanguageVersion) -> Self {
        Self::for_release(
            crate::kotlin_version::target(),
            language_version,
            api_version,
        )
    }

    /// [`Self::for_versions`] under an explicitly chosen reference release, for a driver that
    /// configures a compilation before fixing the process-wide reference version.
    pub fn for_release(
        release: KotlinVersion,
        language_version: LanguageVersion,
        api_version: LanguageVersion,
    ) -> Self {
        let enabled = FeatureTable::for_version(release)
            .unwrap_or_else(|| panic!("kotlinc {release} has no language feature table"))
            .features()
            .iter()
            .filter(|feature| feature.is_enabled_by_default(language_version, api_version))
            .map(|feature| feature.name.clone())
            .collect();
        Self {
            release,
            language_version,
            api_version,
            enabled,
            specific: BTreeMap::new(),
            opted_in: BTreeSet::new(),
        }
    }

    /// Whether kotlinc marks code compiled with these settings as pre-release
    /// (`LanguageVersionSettings.isPreRelease`): the language version is experimental, or an
    /// explicitly enabled feature has not been released by a stable language version.
    pub fn is_pre_release(&self) -> bool {
        let versions = self.versions();
        if !versions.is_stable(self.language_version) {
            return true;
        }
        let table = self.table();
        self.specific.iter().any(|(name, &enabled)| {
            enabled
                && table.get(name).is_some_and(|feature| {
                    feature.forces_pre_release_binaries_if_enabled(versions, self.language_version)
                })
        })
    }
}

/// The kotlinc `LanguageFeature`s krusty's frontend or backend consumes, sorted by name, one per
/// line. A feature is listed once some phase checks it (`LangFeatures::has`), so a command line may
/// turn it on and get that behaviour.
const MODELED_FEATURES: &[&str] = &[
    "AllowAccessToProtectedFieldFromSuperCompanion",
    "AnnotationsInMetadata",
    "BareArrayClassLiteral",
    "CompanionBlocksAndExtensions",
    "ContextParameters",
    "ContextReceivers",
    "ContextSensitiveResolutionUsingExpectedType",
    "DataClassCopyRespectsConstructorVisibility",
    "EagerLambdaAnalysis",
    "EnableNameBasedDestructuringShortForm",
    "EnumEntries",
    "ExplicitBackingFields",
    "ExplicitContextArguments",
    "FullValueClasses",
    "ImplicitSignedToUnsignedIntegerConversion",
    "JvmSupportRecursiveTypeOf",
    "LocalTypeAliases",
    "MultiDollarInterpolation",
    "MultiPlatformProjects",
    "NameBasedDestructuring",
    "NestedTypeAliases",
    "PrioritizedEnumEntries",
    "UnitConversionsOnArbitraryExpressions",
    "UnnamedLocalVariables",
    "WhenGuards",
];

impl LangFeatures {
    /// The feature table of the reference release these defaults come from.
    pub fn table(&self) -> &'static FeatureTable {
        FeatureTable::for_version(self.release)
            .unwrap_or_else(|| panic!("kotlinc {} has no language feature table", self.release))
    }

    /// The language version policy of the reference release these defaults come from.
    pub fn versions(&self) -> &'static LanguageVersionPolicy {
        LanguageVersionPolicy::for_release(self.release)
            .unwrap_or_else(|| panic!("kotlinc {} has no language version policy", self.release))
    }

    /// Whether krusty's frontend/backend has an explicit semantic consumer for this kotlinc
    /// `LanguageFeature`. Command-line boundaries use this to refuse toggles that would otherwise
    /// be recorded by name without changing compilation semantics.
    pub fn models(name: &str) -> bool {
        MODELED_FEATURES.binary_search(&name).is_ok()
    }

    /// Whether `name` is on at this language/API level before any explicit setting
    /// (kotlinc's `isEnabledByDefault`).
    pub fn is_enabled_by_default(&self, name: &str, api_version: LanguageVersion) -> bool {
        self.table().get(name).is_some_and(|feature| {
            feature.is_enabled_by_default(self.language_version, api_version)
        })
    }

    /// The public source-language level from which this feature baseline was derived.
    ///
    /// Ordered feature overrides do not replace the language level: consumers that implement a
    /// language-level semantic transition must read this fact instead of introducing a second
    /// compiler-specific switch.
    pub const fn language_version(&self) -> LanguageVersion {
        self.language_version
    }

    /// The API level these defaults were derived for.
    pub const fn api_version(&self) -> LanguageVersion {
        self.api_version
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `name` (a kotlinc `LanguageFeature` identifier) is enabled.
    pub fn has(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn enable(&mut self, name: &str) {
        self.set(name, true);
    }

    pub fn disable(&mut self, name: &str) {
        self.set(name, false);
    }

    /// Record an explicit feature state, overriding the language-version default.
    pub fn set(&mut self, name: &str, enabled: bool) {
        if enabled {
            self.enabled.insert(name.to_string());
        } else {
            self.enabled.remove(name);
        }
        self.specific.insert(name.to_string(), enabled);
    }

    /// The reference release whose feature table supplies the defaults.
    pub fn release(&self) -> KotlinVersion {
        self.release
    }

    /// The explicitly set features and their states, by name.
    pub fn explicit_settings(&self) -> impl Iterator<Item = (&str, bool)> {
        self.specific
            .iter()
            .map(|(name, &enabled)| (name.as_str(), enabled))
    }

    /// Whether `name` was set explicitly.
    pub fn is_explicit(&self, name: &str) -> bool {
        self.specific.contains_key(name)
    }

    /// Whether both enable exactly the same features, whatever was set explicitly.
    pub fn enables_same_features(&self, other: &Self) -> bool {
        self.enabled == other.enabled
    }

    /// Enable every feature enabled in `other`.
    pub fn extend(&mut self, other: &Self) {
        self.enabled.extend(other.enabled.iter().cloned());
        for (name, &enabled) in &other.specific {
            if enabled {
                self.specific.insert(name.clone(), true);
            }
        }
        self.opted_in.extend(other.opted_in.iter().cloned());
    }

    /// Accept the opt-in requirement of the marker with this dotted fully qualified name module-wide,
    /// as kotlinc's `-opt-in=<fq name>` does.
    pub fn opt_in(&mut self, marker: &str) {
        self.opted_in.insert(marker.to_string());
    }

    /// The dotted fully qualified names of the markers accepted module-wide.
    pub fn opted_in(&self) -> impl Iterator<Item = &str> {
        self.opted_in.iter().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.enabled.iter().map(String::as_str)
    }

    /// Apply the payload of a `// LANGUAGE:` test directive: whitespace- or comma-separated
    /// `+Feature` / `-Feature` tokens (`+` enables, `-` disables). Command-line arguments are read
    /// by kotlinc's rules in the command line's `language_settings`, never here.
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
            // `// EXPLICIT_API_MODE: STRICT|WARNING` — kotlinc's `-Xexplicit-api` analysis mode,
            // carried the same way.
            if let Some(rest) = l.strip_prefix("// EXPLICIT_API_MODE:") {
                self.apply_explicit_api_mode(&rest.trim().to_ascii_lowercase());
            }
            // `// OPT_IN: a.B, c.D` — the test infrastructure's spelling of `-opt-in` per marker.
            if let Some(rest) = l.strip_prefix("// OPT_IN:") {
                for marker in rest.split([' ', ',', '\t']).filter(|s| !s.is_empty()) {
                    self.opt_in(marker);
                }
            }
        }
    }

    /// Select kotlinc's explicit API mode (`strict`, `warning`, or `disable`), an analysis mode
    /// rather than a language feature. Returns `false` for any other spelling.
    pub fn apply_explicit_api_mode(&mut self, mode: &str) -> bool {
        self.disable("ExplicitApiStrict");
        self.disable("ExplicitApiWarning");
        match mode {
            "strict" => self.enable("ExplicitApiStrict"),
            "warning" => self.enable("ExplicitApiWarning"),
            "disable" => {}
            _ => return false,
        }
        true
    }

    /// Collect every `// LANGUAGE:` directive in a source file.
    pub fn from_source(src: &str) -> Self {
        let mut f = Self::default();
        f.apply_source_directives(src);
        f
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
    fn modeled_features_are_sorted_kotlinc_features() {
        assert!(MODELED_FEATURES.windows(2).all(|pair| pair[0] < pair[1]));
        let table =
            FeatureTable::for_version(crate::kotlin_version::KotlinVersion::newest()).unwrap();
        for name in MODELED_FEATURES {
            assert!(
                table.get(name).is_some(),
                "{name} is not a kotlinc language feature"
            );
        }
    }

    #[test]
    fn modeled_features_are_the_frontend_and_backend_capability_boundary() {
        assert!(LangFeatures::models("ContextParameters"));
        assert!(LangFeatures::models("FullValueClasses"));
        assert!(!LangFeatures::models(
            "AllowEagerSupertypeAccessibilityChecks"
        ));
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
    fn an_older_language_level_enables_only_features_stable_by_that_level() {
        let features = LangFeatures::for_language_version(LanguageVersion::V2_2);
        for name in [
            "AllowAccessToProtectedFieldFromSuperCompanion",
            "BareArrayClassLiteral",
            "EnumEntries",
            "MultiDollarInterpolation",
            "PrioritizedEnumEntries",
            "WhenGuards",
        ] {
            assert!(features.has(name), "{name} is stable by 2.2");
        }
        for name in [
            "ContextParameters",
            "ExplicitBackingFields",
            "ExplicitContextArguments",
            "NameBasedDestructuring",
        ] {
            assert!(!features.has(name), "{name} is not stable in 2.2");
        }
    }

    #[test]
    fn experimental_language_levels_enable_their_standard_feature_baseline() {
        let features = LangFeatures::for_language_version(LanguageVersion::V2_5);
        assert!(features.has("ExplicitContextArguments"));
        assert!(features.has("NameBasedDestructuring"));
        assert!(features.has("ContextParameters"));
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

    #[test]
    fn an_older_api_version_keeps_api_gated_features_off() {
        // `EnumEntries` is a language 1.9 feature that also needs API 1.8; every supported API
        // reaches it. `AnnotationsInMetadata` needs language 2.4 and API 1.0.
        let features = LangFeatures::for_versions(LanguageVersion::V2_4, LanguageVersion::V2_0);
        assert!(features.has("EnumEntries"));
        assert!(features.has("AnnotationsInMetadata"));
        let older = LangFeatures::for_versions(LanguageVersion::V2_2, LanguageVersion::V2_2);
        assert!(!older.has("AnnotationsInMetadata"));
    }

    #[test]
    fn pre_release_follows_the_language_version_and_explicit_unreleased_features() {
        assert!(!LangFeatures::for_language_version(LanguageVersion::V2_4).is_pre_release());
        assert!(LangFeatures::for_language_version(LanguageVersion::V2_5).is_pre_release());
        let mut features = LangFeatures::new();
        features.apply_directive("+NameBasedDestructuring");
        assert!(!features.is_pre_release(), "no pre-release marker");
        features.apply_directive("+CompanionBlocksAndExtensions");
        assert!(features.is_pre_release());
        features.apply_directive("-CompanionBlocksAndExtensions");
        assert!(
            !features.is_pre_release(),
            "a disabled feature forces nothing"
        );
    }
}
