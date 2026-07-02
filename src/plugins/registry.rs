//! Extension **registration** — the layer that records which compiler plugins krusty knows about *in
//! general*, independent of any one compilation. This mirrors kotlinc's two-level model:
//!
//!   1. REGISTRATION (this module) — like a plugin's `CompilerPluginRegistrar` declaring its
//!      extensions to the compiler. krusty's registry maps a kotlinc plugin id to either a NATIVE
//!      reimplementation (an `IrPlugin` it runs in-process) or a CODEGEN HOST (KSP, run via sidecar).
//!   2. ACTIVATION (`cli::PluginConfig`) — the per-compilation `-Xplugin`/`-P` switches that turn
//!      registered extensions on for *this* unit, with options.
//!
//! `resolve` joins the two: registry × per-unit config → the plugins to actually run, plus
//! **diagnostics** that make krusty's behavior reliable as a drop-in:
//!
//!   - a NATIVE-reimplemented plugin (serialization) → INFO that krusty substitutes its own
//!     ABI-matched implementation and does NOT execute the supplied JVM compiler-plugin jar (krusty
//!     cannot run FIR/IR plugins);
//!   - a HOSTED plugin (KSP) → INFO that the real jar runs via the sidecar;
//!   - an `-Xplugin` jar krusty neither reimplements nor can host (Compose, any third-party FIR/IR
//!     plugin) → ERROR. Silently ignoring it would emit wrong bytecode, so a drop-in must fail loudly.

use crate::plugins::cli::{PluginConfig, KSP_PLUGIN_ID, SERIALIZATION_PLUGIN_ID};
use crate::plugins::serialization::{SerializationAbi, SerializationPlugin};
use crate::plugins::{IrPlugin, PluginHost};

/// Per-compilation context handed to a native extension's builder.
pub struct Activation<'a> {
    pub config: &'a PluginConfig,
    /// `-classpath` jars — a native plugin reads its target runtime version from here (drop-in: no flag).
    pub classpath: &'a [String],
    pub module_name: &'a str,
}

/// Builds the native `IrPlugin` for an extension, configured from the activation context.
type NativeBuilder = fn(&Activation) -> Box<dyn IrPlugin>;

/// What a registered extension *is* to krusty.
pub enum ExtensionKind {
    /// krusty reimplements it natively as an in-process IR pass (FIR/IR plugins it can't run as jars).
    Native(NativeBuilder),
    /// krusty hosts the real plugin out-of-process (codegen-only: KSP/APT).
    CodegenHost,
}

/// One known extension: the kotlinc plugin id it answers to, the `-Xplugin` jar substring that
/// identifies it, and how krusty realizes it.
pub struct RegisteredExtension {
    pub plugin_id: &'static str,
    pub jar_marker: &'static str,
    pub kind: ExtensionKind,
}

/// A diagnostic emitted while resolving plugins for a compilation. The driver forwards these to the
/// normal `DiagSink`; `is_error()` ones must fail the compile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginDiagnostic {
    /// krusty ran its native implementation instead of the supplied JVM compiler-plugin jar.
    NativeSubstitution {
        plugin_id: String,
        jar: Option<String>,
    },
    /// krusty will host the real plugin via the sidecar.
    Hosted { plugin_id: String },
    /// A plugin krusty can neither reimplement nor host — hard error. `plugin` is the offending
    /// `-Xplugin` jar path, or `plugin id '<id>'` when activated only via `-P` with no jar.
    Unsupported { plugin: String },
}

impl PluginDiagnostic {
    pub fn is_error(&self) -> bool {
        matches!(self, PluginDiagnostic::Unsupported { .. })
    }

    pub fn message(&self) -> String {
        match self {
            PluginDiagnostic::NativeSubstitution { plugin_id, jar } => format!(
                "krusty: '{plugin_id}' is handled by krusty's built-in, ABI-matched implementation; \
                 the supplied compiler-plugin jar{} is not executed (krusty cannot run JVM FIR/IR \
                 plugins and substitutes a native pass).",
                jar.as_deref().map(|j| format!(" '{j}'")).unwrap_or_default()
            ),
            PluginDiagnostic::Hosted { plugin_id } => {
                format!("krusty: hosting '{plugin_id}' via the KSP sidecar (the real plugin runs unmodified).")
            }
            PluginDiagnostic::Unsupported { plugin } => format!(
                "krusty: unsupported compiler plugin '{plugin}': krusty has no native implementation \
                 for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). \
                 Ignoring it would silently produce wrong output, so this is an error — remove the \
                 plugin or compile this module with kotlinc."
            ),
        }
    }
}

/// The plugins to actually run for a compilation, plus the diagnostics describing what krusty did.
pub struct Resolved {
    pub native: PluginHost,
    pub ksp_active: bool,
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl Resolved {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// Build the native serialization plugin, reading its target ABI from the classpath runtime jar.
fn build_serialization(act: &Activation) -> Box<dyn IrPlugin> {
    let abi = SerializationAbi::from_classpath(act.classpath).unwrap_or_default();
    Box::new(SerializationPlugin::new(abi, act.module_name.to_string()))
}

/// The set of extensions krusty knows about — independent of any compilation.
#[derive(Default)]
pub struct PluginRegistry {
    extensions: Vec<RegisteredExtension>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The extensions krusty ships with: serialization (native reimpl) + KSP (codegen host).
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        r.register(RegisteredExtension {
            plugin_id: SERIALIZATION_PLUGIN_ID,
            jar_marker: "serialization",
            kind: ExtensionKind::Native(build_serialization),
        });
        r.register(RegisteredExtension {
            plugin_id: KSP_PLUGIN_ID,
            jar_marker: "symbol-processing",
            kind: ExtensionKind::CodegenHost,
        });
        r
    }

    pub fn register(&mut self, ext: RegisteredExtension) {
        self.extensions.push(ext);
    }

    pub fn is_registered(&self, plugin_id: &str) -> bool {
        self.extensions.iter().any(|e| e.plugin_id == plugin_id)
    }

    /// Join registration with the per-compilation switches: build the active native plugins, flag KSP,
    /// and emit diagnostics (including a hard error for any `-Xplugin` jar krusty can't honor).
    pub fn resolve(&self, act: &Activation) -> Resolved {
        let mut native = PluginHost::new();
        let mut ksp_active = false;
        let mut diagnostics = Vec::new();

        for ext in &self.extensions {
            if !act.config.activates(ext.jar_marker, ext.plugin_id) {
                continue;
            }
            match ext.kind {
                ExtensionKind::Native(build) => {
                    native.register(build(act));
                    diagnostics.push(PluginDiagnostic::NativeSubstitution {
                        plugin_id: ext.plugin_id.to_string(),
                        jar: jar_matching(act.config, ext.jar_marker),
                    });
                }
                ExtensionKind::CodegenHost => {
                    ksp_active = true;
                    diagnostics.push(PluginDiagnostic::Hosted {
                        plugin_id: ext.plugin_id.to_string(),
                    });
                }
            }
        }

        // Any -Xplugin jar that matches no registered extension is a FIR/IR plugin krusty can neither
        // run nor substitute — fail rather than silently mis-compile.
        for jar in &act.config.plugin_jars {
            let name = basename(jar);
            if !self.extensions.iter().any(|e| name.contains(e.jar_marker)) {
                diagnostics.push(PluginDiagnostic::Unsupported {
                    plugin: jar.clone(),
                });
            }
        }

        // ...and a plugin activated purely via `-P plugin:<id>:…` (no jar) for an UNregistered id is
        // equally unsupported — Compose wired only by id must not slip through silently.
        let mut flagged_ids: Vec<&str> = Vec::new();
        for opt in &act.config.options {
            let known = self.extensions.iter().any(|e| e.plugin_id == opt.id);
            if !known && !flagged_ids.contains(&opt.id.as_str()) {
                flagged_ids.push(&opt.id);
                diagnostics.push(PluginDiagnostic::Unsupported {
                    plugin: format!("plugin id '{}'", opt.id),
                });
            }
        }

        Resolved {
            native,
            ksp_active,
            diagnostics,
        }
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn jar_matching(config: &PluginConfig, marker: &str) -> Option<String> {
    config
        .plugin_jars
        .iter()
        .find(|j| basename(j).contains(marker))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> PluginConfig {
        PluginConfig::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    fn activation<'a>(config: &'a PluginConfig, classpath: &'a [String]) -> Activation<'a> {
        Activation {
            config,
            classpath,
            module_name: "app",
        }
    }

    #[test]
    fn builtins_register_serialization_and_ksp() {
        let r = PluginRegistry::with_builtins();
        assert!(r.is_registered(SERIALIZATION_PLUGIN_ID));
        assert!(r.is_registered(KSP_PLUGIN_ID));
        assert!(!r.is_registered("androidx.compose.compiler.plugins.kotlin"));
    }

    #[test]
    fn serialization_resolves_to_native_with_substitution_info() {
        let c = cfg(&["-Xplugin=/k/kotlinx-serialization-compiler-plugin.jar"]);
        let cp = vec!["/k/kotlinx-serialization-core-jvm-1.8.1.jar".to_string()];
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &cp));

        assert_eq!(
            resolved.native.plugin_names(),
            vec!["kotlinx.serialization"]
        );
        assert!(!resolved.ksp_active);
        assert!(!resolved.has_errors());
        // INFO: krusty substituted its own implementation, not the supplied jar.
        assert!(matches!(
            resolved.diagnostics.as_slice(),
            [PluginDiagnostic::NativeSubstitution { plugin_id, jar: Some(_) }] if plugin_id == SERIALIZATION_PLUGIN_ID
        ));
    }

    #[test]
    fn ksp_resolves_to_host() {
        let c = cfg(&[
            "-Xplugin=/k/symbol-processing.jar",
            "-P",
            "plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/p/proc.jar",
        ]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.ksp_active);
        assert!(resolved.native.is_empty());
        assert!(!resolved.has_errors());
        assert!(matches!(
            resolved.diagnostics.as_slice(),
            [PluginDiagnostic::Hosted { plugin_id }] if plugin_id == KSP_PLUGIN_ID
        ));
    }

    #[test]
    fn unsupported_plugin_is_a_hard_error() {
        // Compose: a FIR/IR plugin krusty neither reimplements nor can host → must error, not ignore.
        let c = cfg(&["-Xplugin=/k/compose-compiler-plugin.jar"]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert!(
            resolved.has_errors(),
            "unknown FIR/IR plugin must fail the compile"
        );
        let msg = resolved.diagnostics[0].message();
        assert!(msg.contains("compose-compiler-plugin.jar"));
        assert!(msg.contains("unsupported"));
    }

    #[test]
    fn unsupported_plugin_via_p_only_also_errors() {
        // Compose wired by id with NO -Xplugin jar must still fail — not slip through silently.
        let c = cfg(&[
            "-P",
            "plugin:androidx.compose.compiler.plugins.kotlin:suppressKotlinVersionCompatibilityCheck=true",
        ]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(
            resolved.has_errors(),
            "unknown -P plugin id must fail the compile"
        );
        assert!(matches!(
            resolved.diagnostics.as_slice(),
            [PluginDiagnostic::Unsupported { plugin }] if plugin.contains("androidx.compose")
        ));
    }

    #[test]
    fn known_plugin_via_p_only_activates_without_jar() {
        // KSP configured by -P with no -Xplugin jar still activates (and is NOT flagged unsupported).
        let c = cfg(&[
            "-P",
            "plugin:com.google.devtools.ksp.symbol-processing:apclasspath=/p/proc.jar",
        ]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.ksp_active);
        assert!(!resolved.has_errors());
    }

    #[test]
    fn no_plugins_resolves_clean() {
        let c = cfg(&["-classpath", "/k/stdlib.jar", "Main.kt"]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert!(!resolved.ksp_active);
        assert!(resolved.diagnostics.is_empty());
    }

    #[test]
    fn third_party_native_extension_can_be_registered() {
        // The registry is OPEN: registering a new native extension makes krusty honor its plugin —
        // proving registration is general, not hardcoded to the two builtins.
        fn build_noop(_: &Activation) -> Box<dyn IrPlugin> {
            struct NoOp;
            impl IrPlugin for NoOp {
                fn name(&self) -> &str {
                    "vendor.noop"
                }
            }
            Box::new(NoOp)
        }
        let mut r = PluginRegistry::with_builtins();
        r.register(RegisteredExtension {
            plugin_id: "com.vendor.noop",
            jar_marker: "vendor-noop",
            kind: ExtensionKind::Native(build_noop),
        });
        let c = cfg(&["-Xplugin=/k/vendor-noop-compiler.jar"]);
        let resolved = r.resolve(&activation(&c, &[]));
        assert_eq!(resolved.native.plugin_names(), vec!["vendor.noop"]);
        assert!(!resolved.has_errors());
    }

    #[test]
    fn diagnostic_is_error_only_for_unsupported() {
        let native = PluginDiagnostic::NativeSubstitution {
            plugin_id: "x".into(),
            jar: None,
        };
        let hosted = PluginDiagnostic::Hosted {
            plugin_id: "x".into(),
        };
        let unsupported = PluginDiagnostic::Unsupported { plugin: "x".into() };
        assert!(!native.is_error());
        assert!(!hosted.is_error());
        assert!(unsupported.is_error());
    }

    #[test]
    fn native_substitution_message_mentions_jar_only_when_present() {
        let with_jar = PluginDiagnostic::NativeSubstitution {
            plugin_id: "org.jetbrains.kotlinx.serialization".into(),
            jar: Some("/k/serial.jar".into()),
        }
        .message();
        assert!(with_jar.contains("org.jetbrains.kotlinx.serialization"));
        assert!(with_jar.contains("/k/serial.jar"));
        assert!(with_jar.contains("not executed"));

        let no_jar = PluginDiagnostic::NativeSubstitution {
            plugin_id: "org.jetbrains.kotlinx.serialization".into(),
            jar: None,
        }
        .message();
        assert!(no_jar.contains("org.jetbrains.kotlinx.serialization"));
        // With no jar the interpolation is empty — no stray quoted path.
        assert!(!no_jar.contains(".jar"));
    }

    #[test]
    fn hosted_message_names_the_plugin() {
        let msg = PluginDiagnostic::Hosted {
            plugin_id: KSP_PLUGIN_ID.into(),
        }
        .message();
        assert!(msg.contains(KSP_PLUGIN_ID));
        assert!(msg.contains("sidecar"));
    }

    #[test]
    fn new_registry_is_empty_then_open_to_registration() {
        let mut r = PluginRegistry::new();
        assert!(!r.is_registered(SERIALIZATION_PLUGIN_ID));
        r.register(RegisteredExtension {
            plugin_id: "vendor.x",
            jar_marker: "vendor-x",
            kind: ExtensionKind::CodegenHost,
        });
        assert!(r.is_registered("vendor.x"));
        assert!(!r.is_registered("vendor.y"));
    }

    #[test]
    fn basename_strips_both_path_separators() {
        assert_eq!(basename("/a/b/c.jar"), "c.jar");
        assert_eq!(basename("a\\b\\c.jar"), "c.jar");
        assert_eq!(basename("bare.jar"), "bare.jar");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn jar_matching_finds_by_basename_marker() {
        let c = cfg(&[
            "-Xplugin=/k/kotlinx-serialization-compiler-plugin.jar",
            "-Xplugin=/k/symbol-processing.jar",
        ]);
        assert_eq!(
            jar_matching(&c, "serialization"),
            Some("/k/kotlinx-serialization-compiler-plugin.jar".to_string())
        );
        assert_eq!(
            jar_matching(&c, "symbol-processing"),
            Some("/k/symbol-processing.jar".to_string())
        );
        assert_eq!(jar_matching(&c, "no-such-marker"), None);
    }

    #[test]
    fn duplicate_unsupported_p_ids_are_flagged_once() {
        // The same unknown plugin id appearing in multiple -P options is reported a single time.
        let c = cfg(&[
            "-P",
            "plugin:vendor.unknown:a=1",
            "-P",
            "plugin:vendor.unknown:b=2",
        ]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        let unsupported = resolved
            .diagnostics
            .iter()
            .filter(|d| matches!(d, PluginDiagnostic::Unsupported { .. }))
            .count();
        assert_eq!(unsupported, 1);
    }
}
