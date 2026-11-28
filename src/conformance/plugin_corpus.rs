//! Compiler-plugin box corpora.
//!
//! Every compiler plugin krusty implements may keep its own box corpus beside its code, described
//! by a `boxes/corpus.txt` next to the plugin. The descriptor is the only place that names what a
//! plugin's cases need: the case directive that enables it, the reference compiler plugin jar and
//! options, the runtime jars, the upstream `testData` directory, and the Kotlin releases it covers.
//! The harness reads descriptors through [`all`] and has no per-plugin branches, so a plugin's
//! corpus moves with its code. A plugin's expected-failure and not-applicable inventories live in
//! `expected_failures/` and `expected_not_applicable/` beside its descriptor.
//!
//! A descriptor is `key = value` lines; `#` starts a comment line. Keys:
//!
//! | key | meaning |
//! |---|---|
//! | `plugin` | the plugin's name (required, unique) |
//! | `directive` | the `// <DIRECTIVE>` that makes a case compile with the plugin; the corpus runner puts it on every upstream case, which carries none of its own |
//! | `plugin-jar` | the reference compiler's plugin jar, an [artifact](Artifact) |
//! | `plugin-option` | `<plugin id>:<option>=<value>`, passed as `-P plugin:<id>:<option>`; repeatable |
//! | `runtime` | a jar the case compiles and runs against, an [artifact](Artifact); repeatable |
//! | `upstream` | a `JetBrains/kotlin` directory holding the plugin's cases, read at the tag `v<Kotlin version>`; repeatable |
//! | `support` | a `JetBrains/kotlin` source file every case compiles with, as another file of its module; repeatable |
//! | `stubs` | a directory of Java sources, relative to the descriptor, compiled into a runtime the cases link against |
//! | `check` | what a case must do: `box` (the default) compiles and runs `box()`; `classes` compiles to exactly the classes the reference compiler emits, for upstream cases that are code-generation dumps with no `box()` |
//! | `kotlin` | the inclusive Kotlin release bracket `<min>..<max>`, either end optional |
//!
//! An artifact is one source or several joined by `|` (the first that provisions wins):
//! `dist:<jar>` is a jar of the reference distribution, `maven:<group>:<artifact>:<version>` and
//! `google:<group>:<artifact>:<version>` are pinned Maven Central / Google Maven jars, and
//! `maven-kotlin:<group>:<artifact>` is a Maven Central jar at the reference Kotlin version.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::kotlin_version::KotlinVersion;

/// Every plugin's `boxes` directory, relative to the crate root, with its descriptor text.
const DESCRIPTORS: &[(&str, &str)] = &[
    (
        "src/plugins/atomicfu/boxes",
        include_str!("../plugins/atomicfu/boxes/corpus.txt"),
    ),
    (
        "src/plugins/compose/boxes",
        include_str!("../plugins/compose/boxes/corpus.txt"),
    ),
    (
        "src/plugins/parcelize/boxes",
        include_str!("../plugins/parcelize/boxes/corpus.txt"),
    ),
    (
        "src/plugins/serialization/boxes",
        include_str!("../plugins/serialization/boxes/corpus.txt"),
    ),
];

/// One way of providing a jar. See the module documentation for the spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtifactSource {
    Dist(String),
    Maven {
        group: String,
        artifact: String,
        version: String,
    },
    Google {
        group: String,
        artifact: String,
        version: String,
    },
    MavenAtKotlin {
        group: String,
        artifact: String,
    },
}

/// A jar a plugin needs: the first of its sources that provisions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub sources: Vec<ArtifactSource>,
}

impl Artifact {
    fn parse(text: &str) -> Result<Self, String> {
        let sources = text
            .split('|')
            .map(|source| ArtifactSource::parse(source.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { sources })
    }

    /// The jar, provisioned on first use.
    pub fn resolve(&self) -> Option<PathBuf> {
        self.resolve_or_reason().ok()
    }

    /// [`Self::resolve`] that says why no alternative provided the jar: one reason per source.
    pub fn resolve_or_reason(&self) -> Result<PathBuf, String> {
        let mut reasons = Vec::new();
        for source in &self.sources {
            match source.resolve_or_reason() {
                Ok(jar) => return Ok(jar),
                Err(reason) => reasons.push(reason),
            }
        }
        Err(reasons.join("; "))
    }

    /// The pinned version of the Maven source whose artifact is `artifact`.
    pub fn pinned_version(&self, artifact: &str) -> Option<&str> {
        self.sources.iter().find_map(|source| match source {
            ArtifactSource::Maven {
                artifact: name,
                version,
                ..
            }
            | ArtifactSource::Google {
                artifact: name,
                version,
                ..
            } if name == artifact => Some(version.as_str()),
            _ => None,
        })
    }
}

impl ArtifactSource {
    fn parse(text: &str) -> Result<Self, String> {
        let (kind, rest) = text
            .split_once(':')
            .ok_or_else(|| format!("artifact `{text}` names no source kind"))?;
        let parts: Vec<&str> = rest.split(':').collect();
        let arity = |expected: usize| {
            if parts.len() == expected && parts.iter().all(|part| !part.is_empty()) {
                Ok(())
            } else {
                Err(format!("artifact `{text}` is malformed"))
            }
        };
        match kind {
            "dist" => {
                arity(1)?;
                Ok(Self::Dist(parts[0].to_string()))
            }
            "maven" | "google" => {
                arity(3)?;
                let (group, artifact, version) = (
                    parts[0].to_string(),
                    parts[1].to_string(),
                    parts[2].to_string(),
                );
                Ok(if kind == "maven" {
                    Self::Maven {
                        group,
                        artifact,
                        version,
                    }
                } else {
                    Self::Google {
                        group,
                        artifact,
                        version,
                    }
                })
            }
            "maven-kotlin" => {
                arity(2)?;
                Ok(Self::MavenAtKotlin {
                    group: parts[0].to_string(),
                    artifact: parts[1].to_string(),
                })
            }
            other => Err(format!(
                "artifact `{text}` has unknown source kind `{other}`"
            )),
        }
    }

    fn resolve_or_reason(&self) -> Result<PathBuf, String> {
        use crate::toolchain;
        match self {
            Self::Dist(jar) => toolchain::dist_jar(jar)
                .ok_or_else(|| format!("{jar} is not in the reference distribution")),
            Self::Maven {
                group,
                artifact,
                version,
            } => toolchain::fetch_maven_or_reason(group, artifact, version),
            Self::Google {
                group,
                artifact,
                version,
            } => toolchain::fetch_google_maven_or_reason(group, artifact, version),
            Self::MavenAtKotlin { group, artifact } => {
                toolchain::fetch_maven_or_reason(group, artifact, &toolchain::kotlin_version())
            }
        }
    }
}

/// One `-P plugin:<id>:<option>` the reference compile passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOption {
    pub plugin_id: String,
    pub option: String,
}

/// The inclusive Kotlin release bracket a plugin's corpus covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KotlinBracket {
    pub min: Option<KotlinVersion>,
    pub max: Option<KotlinVersion>,
}

impl KotlinBracket {
    fn parse(text: &str) -> Result<Self, String> {
        let (min, max) = text
            .split_once("..")
            .ok_or_else(|| format!("Kotlin bracket `{text}` is not `<min>..<max>`"))?;
        let end = |end: &str| match end.trim() {
            "" => Ok(None),
            version => KotlinVersion::parse(version)
                .map(Some)
                .ok_or_else(|| format!("Kotlin bracket end `{version}` is not a release")),
        };
        Ok(Self {
            min: end(min)?,
            max: end(max)?,
        })
    }

    pub fn contains(&self, version: KotlinVersion) -> bool {
        self.min.is_none_or(|min| version >= min) && self.max.is_none_or(|max| version <= max)
    }
}

impl std::fmt::Display for KotlinBracket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let end =
            |version: Option<KotlinVersion>| version.map(|v| v.to_string()).unwrap_or_default();
        write!(formatter, "{}..{}", end(self.min), end(self.max))
    }
}

/// What a corpus case must do to pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseCheck {
    /// Compile with the plugin and return `"OK"` from `box()`.
    #[default]
    Box,
    /// Compile to exactly the classes the reference compiler and plugin emit, byte for byte.
    Classes,
}

/// A compiler plugin's box corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCorpus {
    pub plugin: String,
    /// The plugin's `boxes` directory, relative to the crate root; empty for a parsed descriptor
    /// that no registry entry placed.
    pub dir: String,
    pub directive: Option<String>,
    pub plugin_jar: Option<Artifact>,
    pub options: Vec<PluginOption>,
    pub runtime: Vec<Artifact>,
    pub upstream: Vec<String>,
    pub support: Vec<String>,
    pub stubs: Option<String>,
    pub check: CaseCheck,
    pub kotlin: KotlinBracket,
}

impl PluginCorpus {
    /// Parse one descriptor.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut plugin = None;
        let mut directive = None;
        let mut plugin_jar = None;
        let mut options = Vec::new();
        let mut runtime = Vec::new();
        let mut upstream = Vec::new();
        let mut support = Vec::new();
        let mut stubs = None;
        let mut check_text = None;
        let mut kotlin = None;
        let once = |slot: &mut Option<String>, key: &str, value: &str| match slot {
            Some(_) => Err(format!("`{key}` is given twice")),
            None => {
                *slot = Some(value.to_string());
                Ok(())
            }
        };
        let mut jar_text = None;
        let mut bracket_text = None;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .map(|(key, value)| (key.trim(), value.trim()))
                .ok_or_else(|| format!("`{line}` is not `key = value`"))?;
            if value.is_empty() {
                return Err(format!("`{key}` has no value"));
            }
            match key {
                "plugin" => once(&mut plugin, key, value)?,
                "directive" => once(&mut directive, key, value)?,
                "plugin-jar" => once(&mut jar_text, key, value)?,
                "kotlin" => once(&mut bracket_text, key, value)?,
                "stubs" => once(&mut stubs, key, value)?,
                "check" => once(&mut check_text, key, value)?,
                "support" => support.push(value.to_string()),
                "plugin-option" => {
                    let (plugin_id, option) = value
                        .split_once(':')
                        .ok_or_else(|| format!("plugin option `{value}` has no plugin id"))?;
                    options.push(PluginOption {
                        plugin_id: plugin_id.to_string(),
                        option: option.to_string(),
                    });
                }
                "runtime" => runtime.push(Artifact::parse(value)?),
                "upstream" => upstream.push(value.to_string()),
                other => return Err(format!("unknown descriptor key `{other}`")),
            }
        }
        if let Some(text) = jar_text {
            plugin_jar = Some(Artifact::parse(&text)?);
        }
        if let Some(text) = bracket_text {
            kotlin = Some(KotlinBracket::parse(&text)?);
        }
        let check = match check_text.as_deref() {
            None | Some("box") => CaseCheck::Box,
            Some("classes") => CaseCheck::Classes,
            Some(other) => return Err(format!("`check = {other}` is neither `box` nor `classes`")),
        };
        let plugin = plugin.ok_or("a descriptor names no `plugin`")?;
        if plugin_jar.is_none() && !options.is_empty() {
            return Err(format!("plugin `{plugin}` has options but no `plugin-jar`"));
        }
        Ok(Self {
            plugin,
            dir: String::new(),
            directive,
            plugin_jar,
            options,
            runtime,
            upstream,
            support,
            stubs,
            check,
            kotlin: kotlin.unwrap_or_default(),
        })
    }

    /// Whether `src` asks for this plugin through its directive.
    pub fn enabled_by(&self, src: &str) -> bool {
        self.directive
            .as_deref()
            .is_some_and(|directive| super::directive(src, directive))
    }

    /// The reference compiler's plugin jar, provisioned on first use.
    pub fn plugin_jar(&self) -> Option<PathBuf> {
        self.plugin_jar.as_ref()?.resolve()
    }

    /// The runtime jars, provisioned on first use; `None` when any one fails to provision.
    pub fn runtime_jars(&self) -> Option<Vec<PathBuf>> {
        self.runtime.iter().map(Artifact::resolve).collect()
    }

    /// The reference jar of the runtime artifact `artifact` the corpus pins.
    pub fn runtime_version(&self, artifact: &str) -> Option<&str> {
        self.runtime
            .iter()
            .find_map(|runtime| runtime.pinned_version(artifact))
    }

    /// The kotlinc arguments that load the plugin: `-Xplugin` and one `-P` per option.
    pub fn compiler_arguments(&self, compiler: KotlinVersion) -> Result<Vec<String>, String> {
        let name = &self.plugin;
        let directive = self.directive.as_deref().unwrap_or(name);
        if !self.kotlin.contains(compiler) {
            return Err(format!(
                "box directive `// {directive}` is not available for kotlinc {compiler}: the \
                 {name} corpus covers Kotlin {}",
                self.kotlin
            ));
        }
        let jar = self
            .plugin_jar
            .as_ref()
            .ok_or_else(|| format!("the {name} plugin has no reference compiler plugin jar"))?
            .resolve_or_reason()
            .map_err(|reason| {
                format!(
                    "box directive `// {directive}` needs the {name} compiler plugin jar: {reason}"
                )
            })?;
        let mut arguments = vec![format!("-Xplugin={}", jar.display())];
        for option in &self.options {
            arguments.extend([
                "-P".to_string(),
                format!("plugin:{}:{}", option.plugin_id, option.option),
            ]);
        }
        Ok(arguments)
    }
}

/// Every plugin corpus, in descriptor order.
pub fn all() -> &'static [PluginCorpus] {
    static CORPORA: OnceLock<Vec<PluginCorpus>> = OnceLock::new();
    CORPORA.get_or_init(|| {
        let corpora: Vec<PluginCorpus> = DESCRIPTORS
            .iter()
            .map(|(dir, text)| {
                let mut corpus = PluginCorpus::parse(text).unwrap_or_else(|error| {
                    panic!("invalid plugin corpus descriptor {dir}/corpus.txt: {error}")
                });
                corpus.dir = (*dir).to_string();
                corpus
            })
            .collect();
        for (index, corpus) in corpora.iter().enumerate() {
            assert!(
                corpora[..index]
                    .iter()
                    .all(|earlier| earlier.plugin != corpus.plugin),
                "two plugin corpus descriptors name `{}`",
                corpus.plugin
            );
        }
        corpora
    })
}

/// The corpus of the plugin called `plugin`.
pub fn named(plugin: &str) -> &'static PluginCorpus {
    all()
        .iter()
        .find(|corpus| corpus.plugin == plugin)
        .unwrap_or_else(|| panic!("no plugin corpus descriptor names `{plugin}`"))
}

/// The corpora whose directive `src` carries.
pub fn enabled_by(src: &str) -> impl Iterator<Item = &'static PluginCorpus> + '_ {
    all().iter().filter(move |corpus| corpus.enabled_by(src))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_descriptors_parse_with_unique_directives() {
        let directives: Vec<_> = all()
            .iter()
            .filter_map(|c| c.directive.as_deref())
            .collect();
        let mut sorted = directives.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), directives.len());
        assert_eq!(
            all().iter().map(|c| c.plugin.as_str()).collect::<Vec<_>>(),
            ["atomicfu", "compose", "parcelize", "serialization"]
        );
    }

    #[test]
    fn a_descriptor_reads_every_key() {
        let corpus = PluginCorpus::parse(
            "# comment\nplugin = demo\ndirective = WITH_DEMO\nplugin-jar = dist:demo.jar\n\
             plugin-option = demo.id:flag=true\nruntime = maven:g:a:1.0 | dist:a.jar\n\
             upstream = plugins/demo/testData/box\nsupport = plugins/demo/testData/lib.kt\n\
             stubs = stubs\ncheck = classes\nkotlin = 2.4.0..2.4.20\n",
        )
        .unwrap();
        assert_eq!(
            corpus,
            PluginCorpus {
                plugin: "demo".into(),
                dir: String::new(),
                directive: Some("WITH_DEMO".into()),
                plugin_jar: Some(Artifact {
                    sources: vec![ArtifactSource::Dist("demo.jar".into())]
                }),
                options: vec![PluginOption {
                    plugin_id: "demo.id".into(),
                    option: "flag=true".into()
                }],
                runtime: vec![Artifact {
                    sources: vec![
                        ArtifactSource::Maven {
                            group: "g".into(),
                            artifact: "a".into(),
                            version: "1.0".into()
                        },
                        ArtifactSource::Dist("a.jar".into())
                    ]
                }],
                upstream: vec!["plugins/demo/testData/box".into()],
                support: vec!["plugins/demo/testData/lib.kt".into()],
                stubs: Some("stubs".into()),
                check: CaseCheck::Classes,
                kotlin: KotlinBracket {
                    min: Some(KotlinVersion::new(2, 4, 0)),
                    max: Some(KotlinVersion::new(2, 4, 20))
                },
            }
        );
        assert!(corpus.enabled_by("// WITH_DEMO\nfun box() = \"OK\""));
        assert!(!corpus.enabled_by("fun box() = \"OK\""));
        assert_eq!(corpus.runtime_version("a"), Some("1.0"));
    }

    #[test]
    fn malformed_descriptors_are_refused_with_the_offending_text() {
        let refused = |text: &str| PluginCorpus::parse(text).unwrap_err();
        assert_eq!(refused("directive = X\n"), "a descriptor names no `plugin`");
        assert_eq!(
            refused("plugin = a\nstubs = x\nstubs = y\n"),
            "`stubs` is given twice"
        );
        assert_eq!(
            refused("plugin = a\nplugin = b\n"),
            "`plugin` is given twice"
        );
        assert_eq!(refused("plugin = a\nwhat\n"), "`what` is not `key = value`");
        assert_eq!(
            refused("plugin = a\nflavor = x\n"),
            "unknown descriptor key `flavor`"
        );
        assert_eq!(
            refused("plugin = a\nruntime = ftp:x\n"),
            "artifact `ftp:x` has unknown source kind `ftp`"
        );
        assert_eq!(
            refused("plugin = a\nruntime = maven:g:a\n"),
            "artifact `maven:g:a` is malformed"
        );
        assert_eq!(
            refused("plugin = a\ncheck = bytes\n"),
            "`check = bytes` is neither `box` nor `classes`"
        );
        assert_eq!(
            refused("plugin = a\nkotlin = 2.4.0\n"),
            "Kotlin bracket `2.4.0` is not `<min>..<max>`"
        );
        assert_eq!(
            refused("plugin = a\nplugin-option = id:x=1\n"),
            "plugin `a` has options but no `plugin-jar`"
        );
    }

    #[test]
    fn a_bracket_is_inclusive_and_either_end_may_be_open() {
        let bracket = KotlinBracket::parse("2.4.0..2.4.20").unwrap();
        assert!(bracket.contains(KotlinVersion::new(2, 4, 0)));
        assert!(bracket.contains(KotlinVersion::new(2, 4, 20)));
        assert!(!bracket.contains(KotlinVersion::V2_4_21));
        let open = KotlinBracket::parse("2.4.20..").unwrap();
        assert!(!open.contains(KotlinVersion::new(2, 4, 10)));
        assert!(open.contains(KotlinVersion::V2_4_21));
        assert!(KotlinBracket::parse("..")
            .unwrap()
            .contains(KotlinVersion::new(2, 4, 0)));
    }

    #[test]
    fn a_version_outside_the_bracket_refuses_the_directive() {
        let corpus = PluginCorpus::parse(
            "plugin = demo\ndirective = WITH_DEMO\nplugin-jar = dist:demo.jar\nkotlin = 2.4.20..\n",
        )
        .unwrap();
        assert_eq!(
            corpus.compiler_arguments(KotlinVersion::new(2, 4, 0)),
            Err(
                "box directive `// WITH_DEMO` is not available for kotlinc 2.4.0: the demo corpus \
                 covers Kotlin 2.4.20.."
                    .to_string()
            )
        );
    }
}
