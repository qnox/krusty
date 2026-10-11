//! kotlinc's JVM argument surface, one table per supported reference release.
//!
//! Each `releases/<version>.tsv` is the output of `scripts/kotlinc-arguments/DumpKotlincArguments.java`
//! run against that release's `kotlin-compiler.jar`: it reflects over the compiler's own
//! `@Argument`, `@Enables`/`@Disables` and `@Deprecated` annotations. The parser reads these facts
//! instead of re-spelling them, so a name, alias, value type, delimiter or lifecycle is never copied
//! by hand. Every release has its own table, even when it equals another release's. Regenerate a
//! table with `just kotlinc-arguments <version>`.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use krusty::kotlin_version::KotlinVersion;

/// The release tables, by reference version.
/// `every_supported_release_has_a_table` keeps this list equal to the
/// `kotlin-versions` manifest, and `every_release_reads_its_own_file` keeps each entry on the file
/// named after its release.
macro_rules! releases {
    ($($version:ident => $file:literal),+ $(,)?) => {
        const RELEASES: &[(KotlinVersion, &str)] = &[$((
            KotlinVersion::$version,
            include_str!(concat!("releases/", $file, ".tsv")),
        )),+];
        #[cfg(test)]
        const RELEASE_FILES: &[(KotlinVersion, &str)] = &[$((KotlinVersion::$version, $file)),+];
    };
}

releases! {
    V2_4_0 => "2.4.0",
    V2_4_10 => "2.4.10",
    V2_4_20 => "2.4.20",
}

/// The Kotlin type of an argument's field, which decides how kotlinc reads its value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// `Boolean`: present means `true`; `=true` and `=false` are the only explicit values.
    Bool,
    /// `String`: one value, the last occurrence wins.
    String,
    /// `Array<String>`: occurrences accumulate, each value split on the delimiter.
    Array,
}

/// `Argument.delimiter`, which splits an array argument's value into elements.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delimiter {
    Comma,
    PathSeparator,
    Space,
    Semicolon,
    /// `Argument.Delimiters.none`: the whole value is one element.
    Whole,
}

impl Delimiter {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "," => Self::Comma,
            "<path_separator>" => Self::PathSeparator,
            " " => Self::Space,
            ";" => Self::Semicolon,
            "" => Self::Whole,
            _ => return None,
        })
    }

    pub fn split(self, value: &str) -> Vec<String> {
        let separator = match self {
            Self::Comma => ",",
            Self::PathSeparator => {
                if cfg!(windows) {
                    ";"
                } else {
                    ":"
                }
            }
            Self::Space => " ",
            Self::Semicolon => ";",
            Self::Whole => return vec![value.to_string()],
        };
        value.split(separator).map(str::to_string).collect()
    }
}

/// One `@Enables`/`@Disables` entry: the language feature an argument toggles, and the string value
/// it toggles it for (`None` for a boolean argument).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeatureToggle {
    pub feature: String,
    pub if_value_is: Option<String>,
}

/// Where kotlinc declares the argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Origin {
    /// A field of `K2JVMCompilerArguments` or one of its superclasses.
    Current,
    /// `RemovedCompilerArguments`: still parsed, so a value is consumed, but only warned about.
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArgumentSpec {
    /// `Argument.value`, the canonical spelling (`-classpath`, `-Xfriend-paths`).
    pub name: String,
    /// `Argument.shortName` (`-cp`). Only the bare form is accepted: `-cp=x` is an invalid argument.
    pub short_name: Option<String>,
    /// `Argument.deprecatedName` (`-Xopt-in` for `-opt-in`), accepted with a warning.
    pub deprecated_name: Option<String>,
    pub kind: ValueKind,
    pub delimiter: Delimiter,
    /// `Argument.deprecatedVersion`, as kotlinc renders it in its warning.
    pub deprecated_in: Option<String>,
    /// `Argument.removedVersion`, as kotlinc renders it in its warning.
    pub removed_in: Option<String>,
    /// `Argument.isObsolete` (releases before 2.4.20): treated exactly like an unknown argument.
    pub obsolete: bool,
    /// The message of the getter's `@kotlin.Deprecated`, appended to the lifecycle warning.
    pub deprecation_message: Option<String>,
    pub enables: Vec<FeatureToggle>,
    pub disables: Vec<FeatureToggle>,
    pub origin: Origin,
}

impl ArgumentSpec {
    /// `Argument.isAdvanced`: an `-X…` name longer than the prefix itself.
    pub fn is_advanced(&self) -> bool {
        self.name.starts_with("-X") && self.name.len() > 2
    }

    /// `ArgumentField.changesLanguageFeatures`.
    pub fn changes_language_features(&self) -> bool {
        !self.enables.is_empty() || !self.disables.is_empty()
    }

    /// The string values an `@Enables`/`@Disables` argument accepts, in declaration order. Empty when
    /// any value is accepted.
    pub fn legal_values(&self) -> Vec<&str> {
        let mut values = Vec::new();
        for toggle in self.enables.iter().chain(&self.disables) {
            if let Some(value) = toggle.if_value_is.as_deref() {
                if !values.contains(&value) {
                    values.push(value);
                }
            }
        }
        values
    }

    fn parse_row(row: &str) -> Result<Self, String> {
        let columns: Vec<&str> = row.split('\t').collect();
        let [name, short, deprecated, kind, delimiter, deprecated_in, removed_in, obsolete, deprecation, enables, disables, origin] =
            columns[..]
        else {
            return Err(format!("expected 12 columns: {row:?}"));
        };
        let optional = |text: &str| (!text.is_empty()).then(|| text.to_string());
        Ok(Self {
            name: name.to_string(),
            short_name: optional(short),
            deprecated_name: optional(deprecated),
            kind: match kind {
                "bool" => ValueKind::Bool,
                "string" => ValueKind::String,
                "array" => ValueKind::Array,
                _ => return Err(format!("unknown value type {kind:?} for {name}")),
            },
            delimiter: Delimiter::parse(delimiter)
                .ok_or_else(|| format!("unknown delimiter {delimiter:?} for {name}"))?,
            deprecated_in: optional(deprecated_in),
            removed_in: optional(removed_in),
            obsolete: obsolete == "obsolete",
            deprecation_message: deprecation
                .split_once(':')
                .and_then(|(_, message)| optional(message)),
            enables: toggles(enables),
            disables: toggles(disables),
            origin: match origin {
                "jvm" => Origin::Current,
                "removed" => Origin::Removed,
                _ => return Err(format!("unknown origin {origin:?} for {name}")),
            },
        })
    }
}

fn toggles(column: &str) -> Vec<FeatureToggle> {
    column
        .split(';')
        .filter(|entry| !entry.is_empty())
        .map(|entry| match entry.split_once('=') {
            Some((feature, value)) => FeatureToggle {
                feature: feature.to_string(),
                if_value_is: Some(value.to_string()),
            },
            None => FeatureToggle {
                feature: entry.to_string(),
                if_value_is: None,
            },
        })
        .collect()
}

/// The arguments one kotlinc release accepts, keyed the way its parser looks them up.
pub struct Catalog {
    pub version: KotlinVersion,
    arguments: Vec<ArgumentSpec>,
    /// `value`, `shortName` and `deprecatedName` of every current argument.
    current: HashMap<String, usize>,
    /// The same keys of every removed argument, consulted only when `current` has no entry.
    removed: HashMap<String, usize>,
}

impl Catalog {
    fn parse(version: KotlinVersion, table: &str) -> Result<Self, String> {
        let arguments = table
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(ArgumentSpec::parse_row)
            .collect::<Result<Vec<_>, _>>()?;
        let mut current = HashMap::new();
        let mut removed = HashMap::new();
        for (index, argument) in arguments.iter().enumerate() {
            let keys = match argument.origin {
                Origin::Current => &mut current,
                Origin::Removed => &mut removed,
            };
            for key in [
                Some(&argument.name),
                argument.short_name.as_ref(),
                argument.deprecated_name.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                if keys.insert(key.clone(), index).is_some() {
                    return Err(format!("{key} names two arguments"));
                }
            }
        }
        Ok(Self {
            version,
            arguments,
            current,
            removed,
        })
    }

    /// The table for one supported reference release.
    pub fn for_version(version: KotlinVersion) -> Option<&'static Catalog> {
        static CATALOGS: OnceLock<BTreeMap<KotlinVersion, Catalog>> = OnceLock::new();
        CATALOGS
            .get_or_init(|| {
                RELEASES
                    .iter()
                    .map(|&(version, table)| {
                        let catalog = Catalog::parse(version, table).unwrap_or_else(|error| {
                            panic!("kotlinc {version} argument table: {error}")
                        });
                        (version, catalog)
                    })
                    .collect()
            })
            .get(&version)
    }

    /// Every argument of this release, current and removed.
    pub fn arguments(&self) -> &[ArgumentSpec] {
        &self.arguments
    }

    /// The argument a command-line key names: a current argument first, then a removed one.
    pub fn lookup(&self, key: &str) -> Option<&ArgumentSpec> {
        self.current
            .get(key)
            .or_else(|| self.removed.get(key))
            .map(|&index| &self.arguments[index])
    }

    pub fn by_name(&self, name: &str) -> Option<&ArgumentSpec> {
        self.lookup(name).filter(|argument| argument.name == name)
    }
}

/// The vendored table text of one release, for comparison with a fresh dump.
pub fn vendored_table(version: KotlinVersion) -> Option<&'static str> {
    RELEASES
        .iter()
        .find_map(|&(release, table)| (release == version).then_some(table))
}

/// The reference releases that have a table.
pub fn release_versions() -> impl Iterator<Item = KotlinVersion> {
    RELEASES.iter().map(|&(version, _)| version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_release_has_a_table() {
        assert_eq!(
            release_versions().collect::<Vec<_>>(),
            KotlinVersion::supported()
        );
        for version in KotlinVersion::supported() {
            let catalog = Catalog::for_version(version).expect("a table");
            assert_eq!(catalog.version, version);
        }
    }

    /// A release never reads another release's file, even one with the same bytes: a later refresh
    /// of one release must not silently change another's policy.
    #[test]
    fn every_release_reads_its_own_file() {
        for &(version, file) in RELEASE_FILES {
            assert_eq!(file, version.to_string());
        }
    }

    /// Spot checks that the table carries the facts the parser relies on, read from the compiler.
    #[test]
    fn the_table_carries_names_aliases_types_and_lifecycle() {
        let catalog = Catalog::for_version(KotlinVersion::V2_4_20).unwrap();
        let classpath = catalog.lookup("-cp").unwrap();
        assert_eq!(classpath.name, "-classpath");
        assert_eq!(classpath.kind, ValueKind::String);
        let opt_in = catalog.lookup("-Xopt-in").unwrap();
        assert_eq!(opt_in.name, "-opt-in");
        assert_eq!(opt_in.kind, ValueKind::Array);
        assert_eq!(opt_in.delimiter, Delimiter::Comma);
        let friends = catalog.by_name("-Xfriend-paths").unwrap();
        assert_eq!(friends.delimiter, Delimiter::Comma);
        let language = catalog.by_name("-XXLanguage").unwrap();
        assert_eq!(language.delimiter, Delimiter::Whole);
        let jvm_default = catalog.by_name("-Xjvm-default").unwrap();
        assert_eq!(jvm_default.deprecated_in.as_deref(), Some("2.2.0"));
        assert_eq!(
            jvm_default.deprecation_message.as_deref(),
            Some("Use `-jvm-default` instead.")
        );
        let use_k2 = catalog.lookup("-Xuse-k2").unwrap();
        assert_eq!(use_k2.origin, Origin::Removed);
        assert_eq!(use_k2.removed_in.as_deref(), Some("2.2.0"));
        let context = catalog.by_name("-Xcontext-parameters").unwrap();
        assert_eq!(context.kind, ValueKind::Bool);
        assert_eq!(
            context.enables,
            vec![FeatureToggle {
                feature: "ContextParameters".to_string(),
                if_value_is: None
            }]
        );
        assert_eq!(
            catalog
                .by_name("-Xname-based-destructuring")
                .unwrap()
                .legal_values(),
            vec!["only-syntax", "name-mismatch", "complete"]
        );

        let older = Catalog::for_version(KotlinVersion::V2_4_0).unwrap();
        assert!(older.lookup("-Xuse-k2").unwrap().obsolete);
        assert!(older.lookup("-Xescaping-functions").is_none());
    }
}
