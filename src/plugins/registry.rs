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
//!   - a HOSTED plugin (KSP) → INFO that the real jar runs via the sidecar — or an ERROR from a driver
//!     that has no codegen host (the kotlinc-compatible command line), which would otherwise drop the
//!     plugin's generated code;
//!   - an `-Xplugin` jar krusty neither reimplements nor can host (Compose, any third-party FIR/IR
//!     plugin) → ERROR. Silently ignoring it would emit wrong bytecode, so a drop-in must fail loudly.

use crate::plugins::cli::{PluginConfig, KSP_PLUGIN_ID, SERIALIZATION_PLUGIN_ID};
use crate::plugins::serialization::{PluginRelease, SerializationAbi, SerializationPlugin};
use crate::plugins::{IrPlugin, PluginHost};

/// Per-compilation context handed to a native extension's builder.
pub struct Activation<'a> {
    pub config: &'a PluginConfig,
    /// `-classpath` jars — a native plugin reads its target runtime version from here (drop-in: no flag).
    pub classpath: &'a [String],
    pub module_name: &'a str,
    /// Whether the driver resolving this compilation runs codegen hosts (KSP). One that does not must
    /// refuse a hosted plugin instead of reporting it hosted and dropping its generated sources.
    pub codegen_host: bool,
}

/// Builds the native `IrPlugin` for an extension, configured from the activation context and the
/// release of the compiler-plugin jar that activated it (see [`declared_release`]), when one did.
type NativeBuilder = fn(&Activation, Option<&str>) -> Box<dyn IrPlugin>;

/// What a registered extension *is* to krusty.
pub enum ExtensionKind {
    /// krusty reimplements it natively as an in-process IR pass (FIR/IR plugins it can't run as jars).
    Native(NativeBuilder),
    /// krusty hosts the real plugin out-of-process (codegen-only: KSP/APT).
    CodegenHost,
}

/// One known extension: the kotlinc plugin id it answers to, the registrar class its jar declares,
/// and how krusty realizes it.
pub struct RegisteredExtension {
    pub plugin_id: &'static str,
    /// The class a plugin jar names in its registrar service file (see [`declared_registrars`]).
    /// kotlinc finds a plugin's entry point through that file, whatever the jar is called, so krusty
    /// recognizes the jar the same way.
    pub registrar: &'static str,
    pub kind: ExtensionKind,
}

/// The service files through which a jar declares a compiler plugin to kotlinc's `ServiceLoader`:
/// the current `CompilerPluginRegistrar`, and the legacy `ComponentRegistrar` that KSP1 still uses.
const REGISTRAR_SERVICES: [&str; 2] = [
    "META-INF/services/org.jetbrains.kotlin.compiler.plugin.CompilerPluginRegistrar",
    "META-INF/services/org.jetbrains.kotlin.compiler.plugin.ComponentRegistrar",
];

/// The registrar classes a plugin classpath entry (a jar or a directory) declares, or `None` when it
/// cannot be read as either. An entry that declares none holds no plugin — a plugin's dependency, say —
/// and kotlinc loads nothing from it.
pub fn declared_registrars(entry: &str) -> Option<Vec<String>> {
    use std::io::Read;
    let path = std::path::Path::new(entry);
    let mut services = Vec::new();
    if path.is_dir() {
        for service in REGISTRAR_SERVICES {
            if let Ok(text) = std::fs::read_to_string(path.join(service)) {
                services.push(text);
            }
        }
    } else {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        for service in REGISTRAR_SERVICES {
            let Ok(mut file) = archive.by_name(service) else {
                continue;
            };
            let mut text = String::new();
            file.read_to_string(&mut text).ok()?;
            services.push(text);
        }
    }
    Some(
        services
            .iter()
            .flat_map(|text| text.lines())
            .map(|line| line.split('#').next().unwrap_or_default().trim())
            .filter(|class| !class.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// The release a plugin classpath entry declares as its manifest's `Implementation-Version`, e.g.
/// `2.4.10-release-377` for the serialization plugin kotlinc 2.4.10 ships. A plugin bundled with
/// kotlinc changes with each kotlinc release, so a native reimplementation follows the jar it was
/// given rather than the Kotlin version krusty targets.
pub fn declared_release(entry: &str) -> Option<String> {
    use std::io::Read;
    const MANIFEST: &str = "META-INF/MANIFEST.MF";
    let path = std::path::Path::new(entry);
    let manifest = if path.is_dir() {
        std::fs::read_to_string(path.join(MANIFEST)).ok()?
    } else {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
        let mut file = archive.by_name(MANIFEST).ok()?;
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        text
    };
    manifest.lines().find_map(|line| {
        let value = line.strip_prefix("Implementation-Version:")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
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
    /// A codegen-host plugin requested from a driver that runs no codegen host — hard error.
    HostUnavailable { plugin_id: String },
    /// A plugin krusty can neither reimplement nor host — hard error. `plugin` is the offending
    /// `-Xplugin` jar path, or `plugin id '<id>'` when activated only via `-P` with no jar.
    Unsupported { plugin: String },
}

impl PluginDiagnostic {
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            PluginDiagnostic::Unsupported { .. } | PluginDiagnostic::HostUnavailable { .. }
        )
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
            PluginDiagnostic::HostUnavailable { plugin_id } => format!(
                "krusty: '{plugin_id}' runs only through krusty's codegen host, which this driver \
                 does not start. Ignoring it would silently drop the code it generates, so this is \
                 an error — run the processor as its own step and compile its generated sources, or \
                 compile this module with kotlinc."
            ),
            PluginDiagnostic::Unsupported { plugin } => format!(
                "krusty: unsupported compiler plugin '{plugin}': krusty has no native implementation \
                 for it and cannot host FIR/IR compiler plugins (only codegen processors via KSP). \
                 Ignoring it would silently produce wrong output, so this is an error — remove the \
                 plugin or compile this module with kotlinc."
            ),
        }
    }
}

/// The native extensions ONE compilation runs. It is resolved once — from the registry and the unit's
/// `-Xplugin`/`-P` switches, or chosen explicitly by an embedder — and every phase that hosts a native
/// plugin (signature collection, body checking, the backend) builds its [`PluginHost`] from this one
/// value, so the phases cannot disagree about what is active.
///
/// Empty by default: kotlinc synthesizes nothing for a plugin it was not given, so neither does krusty.
#[derive(Clone, Debug, Default)]
pub struct NativePlugins {
    enabled: Vec<EnabledNative>,
    config: PluginConfig,
    classpath: Vec<String>,
}

#[derive(Clone, Debug)]
struct EnabledNative {
    plugin_id: &'static str,
    build: NativeBuilder,
    /// The activating jar's [`declared_release`].
    release: Option<String>,
}

impl NativePlugins {
    /// No native extension. `const` so a caller without a compilation can borrow a shared static.
    pub const fn none() -> NativePlugins {
        NativePlugins {
            enabled: Vec::new(),
            config: PluginConfig::none(),
            classpath: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.enabled.is_empty()
    }

    /// The kotlinc plugin ids of the enabled extensions, in registration order.
    pub fn plugin_ids(&self) -> Vec<&'static str> {
        self.enabled.iter().map(|native| native.plugin_id).collect()
    }

    /// Build this compilation's plugin host. `module_name` reaches plugins whose output is
    /// module-mangled (serialization's `write$Self$<module>`).
    pub fn host(&self, module_name: &str) -> PluginHost {
        let activation = Activation {
            config: &self.config,
            classpath: &self.classpath,
            module_name,
            codegen_host: false,
        };
        let mut host = PluginHost::new();
        for native in &self.enabled {
            host.register((native.build)(&activation, native.release.as_deref()));
        }
        host
    }
}

/// The plugins to actually run for a compilation, plus the diagnostics describing what krusty did.
pub struct Resolved {
    pub native: NativePlugins,
    pub ksp_active: bool,
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl Resolved {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }
}

/// Build the native serialization plugin, reading its target ABI from the classpath runtime jar and
/// its diagnostics' wording from the compiler-plugin jar's release.
fn build_serialization(act: &Activation, release: Option<&str>) -> Box<dyn IrPlugin> {
    let abi = SerializationAbi::from_classpath(act.classpath).unwrap_or_default();
    Box::new(
        SerializationPlugin::new(abi, act.module_name.to_string())
            .with_compiler_plugin_release(release.and_then(PluginRelease::parse)),
    )
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
            registrar: "org.jetbrains.kotlinx.serialization.compiler.extensions.SerializationComponentRegistrar",
            kind: ExtensionKind::Native(build_serialization),
        });
        r.register(RegisteredExtension {
            plugin_id: KSP_PLUGIN_ID,
            registrar: "com.google.devtools.ksp.KotlinSymbolProcessingComponentRegistrar",
            kind: ExtensionKind::CodegenHost,
        });
        r
    }

    pub fn register(&mut self, ext: RegisteredExtension) {
        self.extensions.push(ext);
    }

    /// Every registered extension, in registration order.
    pub fn extensions(&self) -> &[RegisteredExtension] {
        &self.extensions
    }

    pub fn is_registered(&self, plugin_id: &str) -> bool {
        self.extensions.iter().any(|e| e.plugin_id == plugin_id)
    }

    /// Every registered native extension, for an analysis that has no plugin switches to read: an
    /// editor over a project model that does not report compiler-plugin classpaths, where resolving
    /// `Foo.serializer()` matters more than kotlinc's exact no-plugin behaviour.
    pub fn every_native_extension(&self) -> NativePlugins {
        NativePlugins {
            enabled: self
                .extensions
                .iter()
                .filter_map(|ext| match ext.kind {
                    ExtensionKind::Native(build) => Some(EnabledNative {
                        plugin_id: ext.plugin_id,
                        build,
                        release: None,
                    }),
                    ExtensionKind::CodegenHost => None,
                })
                .collect(),
            ..NativePlugins::none()
        }
    }

    /// Join registration with the per-compilation switches: select the active native plugins, flag
    /// KSP, and emit diagnostics (including a hard error for any plugin jar krusty can't honor).
    ///
    /// Each requested jar is identified by the registrars it declares, as kotlinc's `ServiceLoader`
    /// identifies it; an extension is active when a requested jar declares its registrar or `-P`
    /// options name its plugin id.
    pub fn resolve(&self, act: &Activation) -> Resolved {
        let mut native = NativePlugins {
            enabled: Vec::new(),
            config: act.config.clone(),
            classpath: act.classpath.to_vec(),
        };
        let mut ksp_active = false;
        let mut diagnostics = Vec::new();
        let jars = act
            .config
            .all_plugin_jars()
            .map(|jar| (jar, declared_registrars(jar)))
            .collect::<Vec<_>>();

        for ext in &self.extensions {
            let jar = jars
                .iter()
                .find(|(_, registrars)| {
                    registrars
                        .iter()
                        .flatten()
                        .any(|registrar| registrar == ext.registrar)
                })
                .map(|(jar, _)| jar.to_string());
            if jar.is_none() && !act.config.configures(ext.plugin_id) {
                continue;
            }
            match ext.kind {
                ExtensionKind::Native(build) => {
                    native.enabled.push(EnabledNative {
                        plugin_id: ext.plugin_id,
                        build,
                        release: jar.as_deref().and_then(declared_release),
                    });
                    diagnostics.push(PluginDiagnostic::NativeSubstitution {
                        plugin_id: ext.plugin_id.to_string(),
                        jar,
                    });
                }
                ExtensionKind::CodegenHost if act.codegen_host => {
                    ksp_active = true;
                    diagnostics.push(PluginDiagnostic::Hosted {
                        plugin_id: ext.plugin_id.to_string(),
                    });
                }
                ExtensionKind::CodegenHost => {
                    diagnostics.push(PluginDiagnostic::HostUnavailable {
                        plugin_id: ext.plugin_id.to_string(),
                    });
                }
            }
        }

        // A jar that declares a registrar no registered extension answers to is a FIR/IR plugin krusty
        // can neither run nor substitute, and one that cannot be read is a plugin krusty cannot even
        // identify — fail rather than silently mis-compile.
        for (jar, registrars) in &jars {
            let understood = registrars.as_ref().is_some_and(|registrars| {
                registrars
                    .iter()
                    .all(|registrar| self.extensions.iter().any(|ext| ext.registrar == registrar))
            });
            if !understood {
                diagnostics.push(PluginDiagnostic::Unsupported {
                    plugin: jar.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: &[&str]) -> PluginConfig {
        PluginConfig::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// A registered extension's registrar, read from the registry so no test spells a plugin's classes.
    fn registrar_of(plugin_id: &str) -> &'static str {
        PluginRegistry::with_builtins()
            .extensions()
            .iter()
            .find(|ext| ext.plugin_id == plugin_id)
            .map(|ext| ext.registrar)
            .expect("a builtin extension")
    }

    const LEGACY_SERVICE: &str = REGISTRAR_SERVICES[1];

    /// A fresh directory for one test's plugin jars.
    fn scratch() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "krusty-plugin-registry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the test directory");
        dir
    }

    /// A jar named `file_name` whose `service` file declares `registrars` (after a license comment, as
    /// real plugin jars carry one). Returns its path.
    fn jar_declaring(file_name: &str, service: &str, registrars: &[&str]) -> String {
        use std::io::Write;
        let path = scratch().join(file_name);
        let mut archive =
            zip::ZipWriter::new(std::fs::File::create(&path).expect("create the test jar"));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive
            .start_file("META-INF/MANIFEST.MF", options)
            .expect("start the manifest");
        archive
            .write_all(b"Manifest-Version: 1.0\n")
            .expect("write the manifest");
        if !registrars.is_empty() {
            archive
                .start_file(service, options)
                .expect("start the service file");
            let text = format!("# Licensed to nobody.\n\n{}\n", registrars.join("\n"));
            archive
                .write_all(text.as_bytes())
                .expect("write the service file");
        }
        archive.finish().expect("finish the test jar");
        path.display().to_string()
    }

    /// A plugin jar declaring `registrars` the current way.
    fn plugin_jar(file_name: &str, registrars: &[&str]) -> String {
        jar_declaring(file_name, REGISTRAR_SERVICES[0], registrars)
    }

    /// A plugin no registry entry answers to: a neutral stand-in for all-open, no-arg, Compose or any
    /// third-party FIR/IR plugin.
    const WIDGET_REGISTRAR: &str = "org.example.widget.WidgetComponentRegistrar";

    fn activation<'a>(config: &'a PluginConfig, classpath: &'a [String]) -> Activation<'a> {
        Activation {
            config,
            classpath,
            module_name: "app",
            codegen_host: true,
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
        let jar = plugin_jar(
            "kotlinx-serialization-compiler-plugin.jar",
            &[registrar_of(SERIALIZATION_PLUGIN_ID)],
        );
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let cp = vec!["/k/kotlinx-serialization-core-jvm-1.8.1.jar".to_string()];
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &cp));

        assert_eq!(resolved.native.plugin_ids(), vec![SERIALIZATION_PLUGIN_ID]);
        assert_eq!(
            resolved.native.host("app").plugin_names(),
            vec!["kotlinx.serialization"]
        );
        assert!(!resolved.ksp_active);
        assert!(!resolved.has_errors());
        // INFO: krusty substituted its own implementation, not the supplied jar.
        assert_eq!(
            resolved.diagnostics,
            vec![PluginDiagnostic::NativeSubstitution {
                plugin_id: SERIALIZATION_PLUGIN_ID.to_string(),
                jar: Some(jar),
            }]
        );
    }

    #[test]
    fn ksp_resolves_to_host() {
        // KSP1 declares its registrar in the legacy `ComponentRegistrar` service file.
        let jar = jar_declaring(
            "symbol-processing-cmdline.jar",
            LEGACY_SERVICE,
            &[registrar_of(KSP_PLUGIN_ID)],
        );
        let c = cfg(&[
            &format!("-Xplugin={jar}"),
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
        // A FIR/IR plugin krusty neither reimplements nor can host → must error, not ignore.
        let jar = plugin_jar("widget-compiler-plugin.jar", &[WIDGET_REGISTRAR]);
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert_eq!(
            resolved.diagnostics,
            vec![PluginDiagnostic::Unsupported {
                plugin: jar.clone()
            }]
        );
        assert!(resolved.has_errors());
        let msg = resolved.diagnostics[0].message();
        assert!(msg.contains(&jar));
        assert!(msg.contains("unsupported"));
    }

    /// kotlinc finds a plugin through the registrar its jar declares, whatever the file is called; so
    /// does krusty. A jar named like the serialization plugin but declaring another registrar is that
    /// other plugin, and the serialization registrar in a jar of any name is serialization.
    #[test]
    fn a_jar_is_recognized_by_the_registrar_it_declares_not_its_name() {
        let impostor = plugin_jar(
            "kotlinx-serialization-compiler-plugin.jar",
            &[WIDGET_REGISTRAR],
        );
        let c = cfg(&[&format!("-Xplugin={impostor}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert_eq!(
            resolved.diagnostics,
            vec![PluginDiagnostic::Unsupported { plugin: impostor }]
        );

        let renamed = plugin_jar("plugin-7.jar", &[registrar_of(SERIALIZATION_PLUGIN_ID)]);
        let c = cfg(&[&format!("-Xplugin={renamed}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert_eq!(resolved.native.plugin_ids(), vec![SERIALIZATION_PLUGIN_ID]);
        assert!(!resolved.has_errors());
    }

    /// One jar may register several plugins; any one krusty cannot run fails the compile even when
    /// another is a plugin it substitutes.
    #[test]
    fn a_jar_declaring_any_unknown_registrar_is_an_error() {
        let jar = plugin_jar(
            "bundle.jar",
            &[registrar_of(SERIALIZATION_PLUGIN_ID), WIDGET_REGISTRAR],
        );
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.has_errors());
        assert!(resolved
            .diagnostics
            .contains(&PluginDiagnostic::Unsupported { plugin: jar }));
    }

    /// A requested entry krusty cannot read cannot be identified, so it cannot be honoured: an error,
    /// never a quiet "no plugin". (kotlinc fails on it too.)
    #[test]
    fn an_unreadable_plugin_entry_is_an_error() {
        let junk = scratch().join("junk.jar");
        std::fs::write(&junk, b"not a zip archive").expect("write the junk jar");
        let junk = junk.display().to_string();
        let missing = scratch().join("absent.jar").display().to_string();
        let c = cfg(&[&format!("-Xplugin={junk},{missing}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert_eq!(
            resolved.diagnostics,
            vec![
                PluginDiagnostic::Unsupported { plugin: junk },
                PluginDiagnostic::Unsupported { plugin: missing },
            ]
        );
    }

    /// A readable jar that declares no registrar holds no plugin — a plugin's dependency jar, say.
    /// kotlinc loads nothing from it and compiles as without it (exit 0, measured on 2.4.20), and so
    /// does krusty: it was read and identified as declaring nothing, not skipped.
    #[test]
    fn a_jar_declaring_no_plugin_loads_nothing() {
        let jar = plugin_jar("support-library.jar", &[]);
        assert_eq!(declared_registrars(&jar), Some(Vec::new()));
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(resolved.native.is_empty());
        assert_eq!(resolved.diagnostics, Vec::new());
    }

    #[test]
    fn declared_release_reads_the_manifest_implementation_version() {
        use std::io::Write;
        let path = scratch().join("released.jar");
        let mut archive =
            zip::ZipWriter::new(std::fs::File::create(&path).expect("create the test jar"));
        archive
            .start_file(
                "META-INF/MANIFEST.MF",
                zip::write::SimpleFileOptions::default(),
            )
            .expect("start the manifest");
        archive
            .write_all(b"Manifest-Version: 1.0\r\nImplementation-Version: 2.4.10-release-377\r\n")
            .expect("write the manifest");
        archive.finish().expect("finish the test jar");
        let path = path.to_string_lossy();
        assert_eq!(
            declared_release(&path).as_deref(),
            Some("2.4.10-release-377")
        );
        let unversioned = plugin_jar("unversioned.jar", &[]);
        assert_eq!(declared_release(&unversioned), None);
    }

    #[test]
    fn declared_registrars_reads_both_service_files_from_a_jar_or_a_directory() {
        let current = plugin_jar("a.jar", &["p.One", "p.Two"]);
        assert_eq!(
            declared_registrars(&current),
            Some(vec!["p.One".to_string(), "p.Two".to_string()])
        );
        let legacy = jar_declaring("b.jar", LEGACY_SERVICE, &["p.Legacy"]);
        assert_eq!(
            declared_registrars(&legacy),
            Some(vec!["p.Legacy".to_string()])
        );

        let dir = scratch();
        let service = dir.join(REGISTRAR_SERVICES[0]);
        std::fs::create_dir_all(service.parent().expect("a services directory"))
            .expect("create the services directory");
        std::fs::write(&service, "p.FromDir # trailing comment\n").expect("write the service file");
        assert_eq!(
            declared_registrars(&dir.display().to_string()),
            Some(vec!["p.FromDir".to_string()])
        );
        assert_eq!(declared_registrars("/definitely/not/there.jar"), None);
    }

    #[test]
    fn unsupported_plugin_via_p_only_also_errors() {
        // An unregistered plugin wired by id with NO -Xplugin jar must still fail — not slip through
        // silently.
        let c = cfg(&["-P", "plugin:org.example.widget:level=3"]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert!(
            resolved.has_errors(),
            "unknown -P plugin id must fail the compile"
        );
        assert!(matches!(
            resolved.diagnostics.as_slice(),
            [PluginDiagnostic::Unsupported { plugin }] if plugin == "plugin id 'org.example.widget'"
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
        fn build_noop(_: &Activation, _: Option<&str>) -> Box<dyn IrPlugin> {
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
            registrar: "com.vendor.noop.NoOpRegistrar",
            kind: ExtensionKind::Native(build_noop),
        });
        let jar = plugin_jar("vendor-plugin.jar", &["com.vendor.noop.NoOpRegistrar"]);
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let resolved = r.resolve(&activation(&c, &[]));
        assert_eq!(
            resolved.native.host("app").plugin_names(),
            vec!["vendor.noop"]
        );
        assert!(!resolved.has_errors());
    }

    /// A driver that starts no codegen host (the kotlinc-compatible command line) must not report KSP
    /// "hosted" and then drop the sources it would have generated.
    #[test]
    fn a_codegen_host_is_an_error_where_no_host_runs() {
        let jar = jar_declaring(
            "symbol-processing.jar",
            LEGACY_SERVICE,
            &[registrar_of(KSP_PLUGIN_ID)],
        );
        let c = cfg(&[&format!("-Xplugin={jar}")]);
        let act = Activation {
            codegen_host: false,
            ..activation(&c, &[])
        };
        let resolved = PluginRegistry::with_builtins().resolve(&act);
        assert!(!resolved.ksp_active);
        assert!(resolved.native.is_empty());
        assert_eq!(
            resolved.diagnostics,
            vec![PluginDiagnostic::HostUnavailable {
                plugin_id: KSP_PLUGIN_ID.to_string()
            }]
        );
        assert!(resolved.has_errors());
    }

    /// `-Xcompiler-plugin` jars get the same verdicts as `-Xplugin` ones.
    #[test]
    fn modern_syntax_jars_are_resolved_like_legacy_ones() {
        let serialization = plugin_jar(
            "kotlinx-serialization-compiler-plugin.jar",
            &[registrar_of(SERIALIZATION_PLUGIN_ID)],
        );
        let widget = plugin_jar("widget-compiler-plugin.jar", &[WIDGET_REGISTRAR]);
        let c = cfg(&[
            &format!("-Xcompiler-plugin={serialization}"),
            &format!("-Xcompiler-plugin={widget}=level=3"),
        ]);
        let resolved = PluginRegistry::with_builtins().resolve(&activation(&c, &[]));
        assert_eq!(resolved.native.plugin_ids(), vec![SERIALIZATION_PLUGIN_ID]);
        assert_eq!(
            resolved.diagnostics,
            vec![
                PluginDiagnostic::NativeSubstitution {
                    plugin_id: SERIALIZATION_PLUGIN_ID.to_string(),
                    jar: Some(serialization),
                },
                PluginDiagnostic::Unsupported { plugin: widget },
            ]
        );
    }

    #[test]
    fn no_native_extension_runs_unless_selected() {
        assert!(NativePlugins::default().is_empty());
        assert!(NativePlugins::none().host("app").is_empty());
        let every = PluginRegistry::with_builtins().every_native_extension();
        assert_eq!(every.plugin_ids(), vec![SERIALIZATION_PLUGIN_ID]);
        assert_eq!(
            every.host("app").plugin_names(),
            vec!["kotlinx.serialization"]
        );
    }

    #[test]
    fn diagnostic_is_error_only_for_plugins_krusty_cannot_run() {
        let native = PluginDiagnostic::NativeSubstitution {
            plugin_id: "x".into(),
            jar: None,
        };
        let hosted = PluginDiagnostic::Hosted {
            plugin_id: "x".into(),
        };
        let unsupported = PluginDiagnostic::Unsupported { plugin: "x".into() };
        let unhostable = PluginDiagnostic::HostUnavailable {
            plugin_id: "x".into(),
        };
        assert!(!native.is_error());
        assert!(!hosted.is_error());
        assert!(unsupported.is_error());
        assert!(unhostable.is_error());
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
            registrar: "vendor.x.Registrar",
            kind: ExtensionKind::CodegenHost,
        });
        assert!(r.is_registered("vendor.x"));
        assert!(!r.is_registered("vendor.y"));
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
