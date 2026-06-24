//! Drop-in extension activation — krusty consumes the **same switches kotlinc does**, so an existing
//! Gradle/Maven build that wires the serialization or KSP compiler plugin works unchanged. There is
//! no krusty-specific plugin registry: plugins are activated by `-Xplugin=<jar>` and configured by
//! `-P plugin:<id>:<key>=<value>`, exactly as `kotlinc` documents.
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

/// The parsed kotlinc plugin switches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginConfig {
    /// `-Xplugin=` jar paths, in order.
    pub plugin_jars: Vec<String>,
    /// `-P plugin:<id>:<key>=<value>` options, in order (keys may repeat, e.g. KSP `apoption`).
    pub options: Vec<PluginOption>,
}

impl PluginConfig {
    /// Parse a kotlinc-style argument list. Recognizes `-Xplugin=<paths>` (`:`-separated, repeatable)
    /// and `-P <spec>` / `-P=<spec>` where `<spec>` is `plugin:<id>:<key>=<value>`. Unknown args are
    /// ignored (the caller handles the rest of the kotlinc CLI).
    pub fn parse(args: &[String]) -> PluginConfig {
        let mut cfg = PluginConfig::default();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(paths) = a.strip_prefix("-Xplugin=") {
                cfg.plugin_jars
                    .extend(paths.split(':').filter(|s| !s.is_empty()).map(String::from));
            } else if let Some(spec) = a.strip_prefix("-P=") {
                cfg.push_spec(spec);
            } else if a == "-P" {
                if let Some(spec) = args.get(i + 1) {
                    cfg.push_spec(spec);
                    i += 1; // consume the spec arg
                }
            }
            i += 1;
        }
        cfg
    }

    /// Parse one `plugin:<id>:<key>=<value>` spec; ignore anything malformed.
    fn push_spec(&mut self, spec: &str) {
        let Some(rest) = spec.strip_prefix("plugin:") else {
            return;
        };
        let Some((id, kv)) = rest.split_once(':') else {
            return;
        };
        // value may itself contain '=' (paths, base64) — split on the first.
        let Some((key, value)) = kv.split_once('=') else {
            return;
        };
        self.options.push(PluginOption {
            id: id.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        });
    }

    fn jar_present(&self, needle: &str) -> bool {
        self.plugin_jars
            .iter()
            .any(|j| j.rsplit('/').next().unwrap_or(j).contains(needle))
    }

    /// Generic per-compilation activation test for a registered extension: its jar is on `-Xplugin`,
    /// or it carries `-P` options under its plugin id. Used by the extension registry.
    pub fn activates(&self, jar_marker: &str, plugin_id: &str) -> bool {
        self.jar_present(jar_marker) || self.options.iter().any(|o| o.id == plugin_id)
    }

    /// Is the serialization compiler plugin on `-Xplugin`?
    pub fn serialization_active(&self) -> bool {
        self.jar_present("serialization")
    }

    /// Is the KSP compiler plugin on `-Xplugin`?
    pub fn ksp_active(&self) -> bool {
        self.jar_present("symbol-processing") || self.options.iter().any(|o| o.id == KSP_PLUGIN_ID)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_xplugin_jars() {
        let cfg = PluginConfig::parse(&args(&[
            "-Xplugin=/k/kotlinx-serialization-compiler-plugin.jar",
            "-Xplugin=/k/symbol-processing.jar:/k/symbol-processing-api.jar",
        ]));
        assert_eq!(cfg.plugin_jars.len(), 3);
        assert!(cfg.serialization_active());
        assert!(cfg.ksp_active());
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
        assert!(cfg.ksp_active());
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

    #[test]
    fn malformed_specs_ignored() {
        let cfg = PluginConfig::parse(&args(&[
            "-P",
            "notplugin:x:y=z",
            "-P=plugin:onlyid",
            "-P=plugin:id:nokeyvalue",
            "src.kt", // an ordinary arg
        ]));
        assert!(cfg.options.is_empty());
        assert!(!cfg.ksp_active());
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
