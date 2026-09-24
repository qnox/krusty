//! Per-compilation extension **activation** — krusty consumes the **same switches kotlinc does**, so
//! an existing Gradle/Maven build that wires the serialization or KSP compiler plugin works unchanged.
//! Plugins are activated by `-Xplugin=<jar>` and configured by `-P plugin:<id>:<key>=<value>`, exactly
//! as `kotlinc` documents. This is layer 2 of two; layer 1 is the general extension registry
//! (`super::registry`), which `resolve` joins with this per-unit config.
//!
//!   -Xplugin=/path/kotlinx-serialization-compiler-plugin.jar
//!   -Xplugin=/path/symbol-processing.jar
//!   -P plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/path/processor.jar
//!   -P plugin:com.google.devtools.ksp.symbol-processing:kspOutputDir=build/generated/ksp
//!
//! This module parses those switches into a [`PluginConfig`]; the driver then maps them onto the
//! native [`super::IrPlugin`] passes / the [`super::ksp`] host. Plugin *versions* are NOT flags —
//! serialization's ABI comes from the `kotlinx-serialization-core` jar on `-classpath` (see
//! [`super::serialization::SerializationAbi::from_classpath`]) and KSP's from its jar coordinate.

/// The real kotlinc plugin ids (the `<id>` in `-P plugin:<id>:...`).
pub const SERIALIZATION_PLUGIN_ID: &str = "org.jetbrains.kotlinx.serialization";
pub const KSP_PLUGIN_ID: &str = "com.google.devtools.ksp.symbol-processing";

/// One `-P plugin:<id>:<key>=<value>` option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOption {
    pub id: String,
    pub key: String,
    pub value: String,
}

/// One `-Xcompiler-plugin=<jar>,<jar>[=<key>=<value>,<key>=<value>]` registration (kotlinc's modern,
/// experimental syntax). Its options carry no plugin id: they configure the plugin those jars load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerPluginSpec {
    pub classpath: Vec<String>,
    pub options: Vec<(String, String)>,
}

/// The parsed kotlinc plugin switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginConfig {
    /// `-Xplugin=` jar paths, in order.
    pub plugin_jars: Vec<String>,
    /// `-P plugin:<id>:<key>=<value>` options, in order (keys may repeat, e.g. KSP `apoption`).
    pub options: Vec<PluginOption>,
    /// `-Xcompiler-plugin=` registrations, in order.
    pub compiler_plugins: Vec<CompilerPluginSpec>,
    /// Switches kotlinc rejects, in its words. A driver must fail on any of them rather than compile
    /// with the plugin configuration it could not read.
    pub errors: Vec<String>,
}

impl PluginConfig {
    /// An empty configuration: no plugin requested. `const` so a provider without a compilation's
    /// switches can hand out a shared static.
    pub const fn none() -> PluginConfig {
        PluginConfig {
            plugin_jars: Vec::new(),
            options: Vec::new(),
            compiler_plugins: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// Parse a kotlinc-style argument list; unknown arguments are skipped (the caller handles the
    /// rest of the kotlinc CLI). See [`PluginConfig::accept`] for the recognized switches.
    pub fn parse(args: &[String]) -> PluginConfig {
        let mut cfg = PluginConfig::default();
        let mut args = args.iter();
        while let Some(argument) = args.next() {
            cfg.accept(argument, || args.next().cloned());
        }
        cfg.finish();
        cfg
    }

    /// Consume one argument if it is a plugin switch, pulling a separate value from `next` when the
    /// switch takes one; returns whether it was. The switches, as kotlinc 2.4 reads them:
    ///
    /// - `-Xplugin=<jar>,<jar>` — repeatable; kotlinc splits the value on `,` only (a `:` is part of
    ///   the path: `-Xplugin=a.jar:b.jar` names one non-existent jar);
    /// - `-P plugin:<id>:<key>=<value>` and `-P=plugin:…`;
    /// - `-Xcompiler-plugin=<jar>,<jar>[=<key>=<value>,…]` — repeatable, one plugin per switch.
    ///
    /// Call [`PluginConfig::finish`] once every argument has been offered.
    pub fn accept(&mut self, argument: &str, next: impl FnOnce() -> Option<String>) -> bool {
        if let Some(paths) = argument.strip_prefix("-Xplugin=") {
            self.plugin_jars.extend(split_paths(paths));
        } else if let Some(spec) = argument.strip_prefix("-Xcompiler-plugin=") {
            let (paths, options) = spec.split_once('=').unwrap_or((spec, ""));
            self.compiler_plugins.push(CompilerPluginSpec {
                classpath: split_paths(paths).collect(),
                options: options
                    .split(',')
                    .filter_map(|option| option.split_once('='))
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
            });
        } else if let Some(spec) = argument.strip_prefix("-P=") {
            self.push_spec(spec);
        } else if argument == "-P" {
            match next() {
                Some(spec) => self.push_spec(&spec),
                None => self
                    .errors
                    .push("no value passed for argument -P".to_string()),
            }
        } else {
            return false;
        }
        true
    }

    /// Checks that need every switch: kotlinc refuses a command line that mixes the legacy
    /// (`-Xplugin`, `-P`) and modern (`-Xcompiler-plugin`) syntaxes.
    pub fn finish(&mut self) {
        let legacy = !self.plugin_jars.is_empty() || !self.options.is_empty();
        if legacy && !self.compiler_plugins.is_empty() {
            self.errors.push(
                "mixing legacy and modern plugin arguments is prohibited. Please use only one \
                 syntax"
                    .to_string(),
            );
        }
    }

    /// Parse one `plugin:<id>:<key>=<value>` spec; anything else is kotlinc's format error.
    fn push_spec(&mut self, spec: &str) {
        let option = spec
            .strip_prefix("plugin:")
            .and_then(|rest| rest.split_once(':'))
            // value may itself contain '=' (paths, base64) — split on the first.
            .and_then(|(id, kv)| kv.split_once('=').map(|(key, value)| (id, key, value)));
        match option {
            Some((id, key, value)) => self.options.push(PluginOption {
                id: id.to_string(),
                key: key.to_string(),
                value: value.to_string(),
            }),
            None => self.errors.push(format!(
                "wrong plugin option format: {spec}, should be \
                 plugin:<pluginId>:<optionName>=<value>"
            )),
        }
    }

    /// Every requested plugin jar, from both syntaxes, in command-line order within each.
    pub fn all_plugin_jars(&self) -> impl Iterator<Item = &str> {
        self.plugin_jars
            .iter()
            .chain(
                self.compiler_plugins
                    .iter()
                    .flat_map(|plugin| plugin.classpath.iter()),
            )
            .map(String::as_str)
    }

    /// Whether `-P` options configure `plugin_id`. The extension registry, which owns every plugin's
    /// identity, activates a registered extension on this or on a requested jar declaring it; this
    /// module knows no plugin by name beyond the ids kotlinc documents.
    pub fn configures(&self, plugin_id: &str) -> bool {
        self.options.iter().any(|option| option.id == plugin_id)
    }

    /// All values for `-P plugin:<id>:<key>` (a key may repeat — KSP `apoption`/`apclasspath`).
    pub fn option_values(&self, id: &str, key: &str) -> Vec<&str> {
        self.options
            .iter()
            .filter(|o| o.id == id && o.key == key)
            .map(|o| o.value.as_str())
            .collect()
    }

    /// The KSP processor classpath (`apclasspath`), the jars KSP scans for `SymbolProcessorProvider`s.
    pub fn ksp_processor_classpath(&self) -> Vec<&str> {
        self.option_values(KSP_PLUGIN_ID, "apclasspath")
    }
}

/// kotlinc's plugin classpath separator is `,` for both syntaxes; empty entries are dropped.
fn split_paths(paths: &str) -> impl Iterator<Item = String> + '_ {
    paths
        .split(',')
        .filter(|path| !path.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_xplugin_jars_split_on_commas_like_kotlinc() {
        let cfg = PluginConfig::parse(&args(&[
            "-Xplugin=/k/kotlinx-serialization-compiler-plugin.jar",
            "-Xplugin=/k/symbol-processing.jar,/k/symbol-processing-api.jar",
        ]));
        assert_eq!(
            cfg.plugin_jars,
            vec![
                "/k/kotlinx-serialization-compiler-plugin.jar",
                "/k/symbol-processing.jar",
                "/k/symbol-processing-api.jar",
            ]
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    }

    /// kotlinc 2.4.20 reads `-Xplugin=a.jar:b.jar` as ONE path ("plugin classpath entry points to a
    /// non-existent location: a.jar:b.jar"); splitting on `:` would also break a Windows `C:\` path.
    #[test]
    fn a_colon_does_not_separate_plugin_jars() {
        let cfg = PluginConfig::parse(&args(&["-Xplugin=/k/a.jar:/k/b.jar"]));
        assert_eq!(cfg.plugin_jars, vec!["/k/a.jar:/k/b.jar"]);
    }

    #[test]
    fn parses_the_modern_compiler_plugin_syntax() {
        let cfg = PluginConfig::parse(&args(&[
            "-Xcompiler-plugin=/k/allopen.jar,/k/extra.jar=annotation=p.Open,preset=spring",
            "-Xcompiler-plugin=/k/noarg.jar",
        ]));
        assert_eq!(
            cfg.compiler_plugins,
            vec![
                CompilerPluginSpec {
                    classpath: vec!["/k/allopen.jar".into(), "/k/extra.jar".into()],
                    options: vec![
                        ("annotation".into(), "p.Open".into()),
                        ("preset".into(), "spring".into()),
                    ],
                },
                CompilerPluginSpec {
                    classpath: vec!["/k/noarg.jar".into()],
                    options: Vec::new(),
                },
            ]
        );
        assert_eq!(
            cfg.all_plugin_jars().collect::<Vec<_>>(),
            vec!["/k/allopen.jar", "/k/extra.jar", "/k/noarg.jar"]
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    }

    /// kotlinc 2.4.20: `error: mixing legacy and modern plugin arguments is prohibited. Please use
    /// only one syntax` for `-Xplugin` or `-P` next to `-Xcompiler-plugin`.
    #[test]
    fn mixing_the_legacy_and_modern_syntaxes_is_an_error() {
        let mixing = "mixing legacy and modern plugin arguments is prohibited. Please use only \
                      one syntax";
        for legacy in [
            &["-Xplugin=/k/a.jar"][..],
            &[
                "-P",
                "plugin:org.jetbrains.kotlin.allopen:annotation=p.Open",
            ][..],
        ] {
            let mut arguments = args(legacy);
            arguments.push("-Xcompiler-plugin=/k/b.jar".to_string());
            assert_eq!(PluginConfig::parse(&arguments).errors, vec![mixing]);
        }
    }

    #[test]
    fn parses_p_options_both_forms() {
        // `-P <spec>` (two args) and `-P=<spec>` (one arg) both work, as kotlinc accepts.
        let cfg = PluginConfig::parse(&args(&[
            "-P",
            "plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/p/proc.jar",
            "-P=plugin:com.google.devtools.ksp.symbol-processing:kspOutputDir=build/ksp",
            "-P",
            "plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/p/proc2.jar",
        ]));
        assert_eq!(
            cfg.ksp_processor_classpath(),
            vec!["/p/proc.jar", "/p/proc2.jar"],
            "apclasspath repeats accumulate"
        );
        assert_eq!(
            cfg.option_values(KSP_PLUGIN_ID, "kspOutputDir"),
            vec!["build/ksp"]
        );
    }

    #[test]
    fn value_may_contain_equals() {
        let cfg = PluginConfig::parse(&args(&[
            "-P",
            "plugin:com.google.devtools.ksp.symbol-processing:apoption=foo=bar=baz",
        ]));
        assert_eq!(
            cfg.option_values(KSP_PLUGIN_ID, "apoption"),
            vec!["foo=bar=baz"]
        );
    }

    /// kotlinc rejects a `-P` value that is not `plugin:<id>:<key>=<value>` (its message prints the
    /// value as `null`; krusty names the value instead) and a `-P` with no value at all.
    #[test]
    fn malformed_specs_are_errors() {
        let cfg = PluginConfig::parse(&args(&[
            "-P",
            "notplugin:x:y=z",
            "-P=plugin:onlyid",
            "-P=plugin:id:nokeyvalue",
            "src.kt", // an ordinary arg
            "-P",
        ]));
        assert!(cfg.options.is_empty());
        assert_eq!(
            cfg.errors,
            vec![
                "wrong plugin option format: notplugin:x:y=z, should be \
                 plugin:<pluginId>:<optionName>=<value>",
                "wrong plugin option format: plugin:onlyid, should be \
                 plugin:<pluginId>:<optionName>=<value>",
                "wrong plugin option format: plugin:id:nokeyvalue, should be \
                 plugin:<pluginId>:<optionName>=<value>",
                "no value passed for argument -P",
            ]
        );
    }

    #[test]
    fn ignores_unrelated_args() {
        let cfg = PluginConfig::parse(&args(&[
            "-classpath",
            "/k/stdlib.jar",
            "-d",
            "out",
            "Main.kt",
        ]));
        assert_eq!(cfg, PluginConfig::default());
    }
}
