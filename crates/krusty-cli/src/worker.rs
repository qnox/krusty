//! Bazel persistent-worker mode, speaking the argument surface of intellij-community's
//! `jvm-inc-builder`.
//!
//! A `jvm_library` target in that build invokes a worker with flags produced by
//! `build/jvm-rules/rules/impl/builder-args.bzl` and `kotlinc-options.bzl` — a vocabulary of its own
//! (`--out`, `--srcs`, `--cp`, `--jvm_default`, `--x_no_param_assertions`, …) rather than kotlinc's
//! command line. This module translates that vocabulary into krusty's own options, so the same
//! target can be built by krusty instead of the embedded K2 pipeline.
//!
//! The translation is the interesting part and it is pure: [`translate`] maps one argument list to a
//! [`WorkUnit`] and is unit-tested against the flags the real build emits. The protocol loop around
//! it is deliberately thin.
//!
//! A mixed target compiles as the builder compiles it: krusty compiles the Kotlin with the `.java`
//! sources read as declarations, then the JDK's javac compiles the Java against krusty's classes
//! ([`java_compilation`]), and both land in the one jar.
//!
//! What krusty CANNOT do here is stated as a failed work response rather than a silently wrong jar:
//! a target with source jars or a `module-info.java` is refused, and krusty produces no reduced ABI
//! jar, so `--abi-out` receives a copy of the full jar.
//! `--resources` groups (`strip_prefix:add_prefix:file`) are copied into the output jar after the
//! classes, under the same entry names the module jar uses.

use std::borrow::Cow;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use krusty_jvm::ir_emit::JvmDefaultMode;

pub mod java_compilation;
mod jps_handoff;

pub use java_compilation::JavaCompilation;
pub use jps_handoff::{FailureKind, Handoff, WorkFailure};

/// One compilation the worker was asked to perform, in krusty's terms.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WorkUnit {
    /// The jar to write (`--out`).
    pub output_jar: PathBuf,
    /// A copy of the output jar (`--abi-out`). krusty emits no reduced ABI jar; a consumer that
    /// compiles against this one therefore rebuilds on any change, not only on ABI changes.
    pub abi_jar: Option<PathBuf>,
    /// Incremental-compilation state file (`--kotlin-cri-out`). krusty keeps no such state, but the
    /// file is a declared action output, so it must exist or bazel fails the action.
    pub cri_file: Option<PathBuf>,
    /// The Kotlin sources.
    pub sources: Vec<PathBuf>,
    /// The `.java` sources, which krusty reads as declarations and javac then compiles.
    pub java: JavaCompilation,
    pub classpath: Vec<PathBuf>,
    pub friend_paths: Vec<PathBuf>,
    pub module_name: Option<String>,
    pub target_label: Option<String>,
    /// The kotlinc-style flags translated from the worker's own option names.
    pub kotlinc_args: Vec<String>,
    /// Worker options that were understood but have no effect on krusty's output.
    pub inert: Vec<String>,
    /// kotlinc-compatible warnings produced while parsing modeled compiler arguments.
    pub argument_warnings: Vec<String>,
    /// Resource groups from `--resources`, in request order. Each group is one
    /// `strip_prefix:add_prefix:file` argument, the spelling `builder-args.bzl` emits.
    pub resources: Vec<ResourceGroup>,
}

impl WorkUnit {
    /// The krusty command line this unit compiles with. The classpath is joined with the separator
    /// the CLI splits it on, so a Windows worker does not read several jars as one path.
    pub fn compiler_arguments(&self) -> Vec<String> {
        let mut argv: Vec<String> = vec!["-d".to_string(), self.output_jar.display().to_string()];
        if let Some(module_name) = &self.module_name {
            argv.push("-module-name".to_string());
            argv.push(module_name.clone());
        }
        if !self.classpath.is_empty() {
            argv.push("-classpath".to_string());
            argv.push(
                self.classpath
                    .iter()
                    .map(|entry| entry.display().to_string())
                    .collect::<Vec<_>>()
                    .join(&crate::cli::CLASSPATH_SEPARATOR.to_string()),
            );
        }
        // kotlinc splits `-Xfriend-paths` on `,`, not on the path separator.
        if !self.friend_paths.is_empty() {
            argv.push(format!(
                "-Xfriend-paths={}",
                self.friend_paths
                    .iter()
                    .map(|entry| entry.display().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        argv.extend(self.kotlinc_args.iter().cloned());
        argv.extend(
            self.sources
                .iter()
                .chain(&self.java.sources)
                .map(|path| path.display().to_string()),
        );
        argv
    }
}

/// One `--resources` argument: files copied into the jar after their strip prefix is removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceGroup {
    /// Directory prefix removed from each file path. Empty keeps the path as the entry name.
    pub strip_prefix: String,
    /// Directory prepended to the stripped path. Empty leaves the stripped path unchanged.
    pub add_prefix: String,
    pub files: Vec<PathBuf>,
}

/// Why a work request cannot be served.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A flag that changes the output shape in a way krusty does not implement.
    Unsupported(String),
    /// The request is malformed (a flag missing its value, no `--out`).
    Malformed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unsupported(detail) => {
                write!(formatter, "unsupported by krusty: {detail}")
            }
            Refusal::Malformed(detail) => write!(formatter, "malformed work request: {detail}"),
        }
    }
}

/// Flags that carry a LIST of values until the next `--flag`.
const LIST_FLAGS: &[&str] = &[
    "--srcs",
    "--src-jars",
    "--cp",
    "--friends",
    "--direct-dependencies",
    "--opt_in",
    "--plugin_options",
    "--x_warning_level",
    "--x_xlanguage",
    "--add-export",
    "--resources",
];

/// Boolean worker options with no value, and how each maps to kotlinc's own spelling. An entry with
/// `None` is understood but changes nothing krusty emits.
fn boolean_option(flag: &str) -> Option<Option<&'static str>> {
    Some(match flag {
        "--progressive" => Some("-progressive"),
        "--x_allow_kotlin_package" => Some("-Xallow-kotlin-package"),
        "--x_allow_result_return_type" => Some("-Xallow-result-return-type"),
        "--x_allow_unstable_dependencies" => Some("-Xallow-unstable-dependencies"),
        "--x_consistent_data_class_copy_visibility" => {
            Some("-Xconsistent-data-class-copy-visibility")
        }
        "--x_context_parameters" => Some("-XXLanguage:+ContextParameters"),
        "--x_explicit_backing_fields" => Some("-Xexplicit-backing-fields"),
        "--x_context_receivers" => Some("-Xcontext-receivers"),
        "--x_inline_classes" => Some("-Xinline-classes"),
        "--x_skip_prerelease_check" => Some("-Xskip-prerelease-check"),
        "--x_when_guards" => Some("-XXLanguage:+WhenGuards"),
        "--x_report_all_warnings" => Some("-Xreport-all-warnings"),
        // Diagnostics and reporting: they change what the compiler PRINTS, not what it emits.
        "--x_render_internal_diagnostic_names" => Some("-Xrender-internal-diagnostic-names"),
        "--trace"
        | "--x_wasm_attach_js_exception"
        | "--x_wasm_generate_closed_world_multimodule"
        | "--x_wasm_kclass_fqn" => None,
        _ => return None,
    })
}

/// Admit only language toggles the frontend actually observes. `LangFeatures` intentionally accepts
/// arbitrary names for the conformance harness, so merely reparsing `-XXLanguage:` cannot prove that
/// a build-request feature changes the compiler. The worker boundary must be stricter: an unknown
/// toggle may change syntax or emitted declarations and therefore cannot be reported as successful.
fn translate_language_features(
    unit: &mut WorkUnit,
    value: &str,
    source_flag: &str,
) -> Result<(), Refusal> {
    let tokens = value.split(',').collect::<Vec<_>>();
    if tokens.iter().any(|token| token.is_empty()) {
        return Err(Refusal::Malformed(format!(
            "{source_flag}: requires at least one +Feature or -Feature"
        )));
    }
    for token in tokens {
        let Some(name) = token.strip_prefix('+').or_else(|| token.strip_prefix('-')) else {
            return Err(Refusal::Malformed(format!(
                "{source_flag} {token}: expected +Feature or -Feature"
            )));
        };
        if name.is_empty() {
            return Err(Refusal::Malformed(format!(
                "{source_flag} {token}: feature name is empty"
            )));
        }
        if krusty::features::LangFeatures::models(name) {
            unit.kotlinc_args.push(format!("-XXLanguage:{token}"));
        } else {
            return Err(Refusal::Unsupported(format!(
                "{source_flag} {token}: language feature is not modeled by krusty"
            )));
        }
    }
    Ok(())
}

/// Whether a forwarded kotlinc argument selects an input or output the structured work request
/// owns. Kotlinc accepts both `-name value` and `-name=value`; checking only the complete token lets
/// the attached form override the structured value when `compile_work_unit` appends these options.
fn is_worker_owned_kotlinc_option(argument: &str) -> bool {
    let name = argument.split_once('=').map_or(argument, |(name, _)| name);
    matches!(
        name,
        "-d" | "-cp" | "-classpath" | "-class-path" | "-module-name"
    ) || argument.starts_with('@')
}

/// Translate one `jvm-inc-builder` argument list into a [`WorkUnit`].
pub fn translate(arguments: &[String]) -> Result<WorkUnit, Refusal> {
    let mut unit = WorkUnit::default();
    let mut java_count: Option<usize> = None;
    let mut warn_off = false;
    let mut raises_warnings = false;
    let mut source_jars = Vec::new();
    let mut index = 0;

    // `--flag value` for the scalar options; a list flag consumes values until the next `--flag`.
    let value_of = |index: usize, flag: &str| -> Result<String, Refusal> {
        arguments
            .get(index + 1)
            .filter(|value| !value.starts_with("--"))
            .cloned()
            .ok_or_else(|| Refusal::Malformed(format!("{flag} requires a value")))
    };

    while index < arguments.len() {
        let flag = arguments[index].as_str();
        if LIST_FLAGS.contains(&flag) {
            let mut values = Vec::new();
            index += 1;
            while index < arguments.len() && !arguments[index].starts_with("--") {
                values.push(arguments[index].clone());
                index += 1;
            }
            match flag {
                "--srcs" => {
                    for value in values {
                        let path = PathBuf::from(&value);
                        if krusty::source::kind(&path) == Some(krusty::source::SourceKind::Java) {
                            // A module declaration puts javac on the module path, which the
                            // builder splits from the classpath; the worker does not model that.
                            if path.file_name().and_then(|name| name.to_str())
                                == Some("module-info.java")
                            {
                                return Err(Refusal::Unsupported(format!(
                                    "--srcs {value}: Java module declarations are not compiled by the worker"
                                )));
                            }
                            unit.java.sources.push(path);
                            continue;
                        }
                        // A script compiles against a script definition the action does not
                        // describe, so the worker leaves it to the reference compiler.
                        if krusty::source::kind(&path) != Some(krusty::source::SourceKind::Kotlin) {
                            return Err(Refusal::Unsupported(format!(
                                "--srcs {value}: the worker compiles only .kt and .java sources"
                            )));
                        }
                        unit.sources.push(path);
                    }
                }
                "--src-jars" => source_jars.extend(values),
                // javac `--add-exports`; it has no effect on the Kotlin half.
                "--add-export" => unit.java.add_exports.extend(values),
                "--cp" => unit.classpath.extend(values.into_iter().map(PathBuf::from)),
                // `associates` — classpath modules whose `internal` declarations this target sees.
                "--friends" => unit
                    .friend_paths
                    .extend(values.into_iter().map(PathBuf::from)),
                // Accepted opt-in markers select diagnostics; the batch CLI checks them.
                "--opt_in" => unit
                    .kotlinc_args
                    .extend(values.into_iter().map(|value| format!("-opt-in={value}"))),
                // Preserve warning policy as the standard kotlinc spelling. The batch parser owns
                // the diagnostic-name registry and duplicate checks, so the worker does not grow
                // a second, drifting copy.
                "--x_warning_level" => {
                    if values.is_empty() {
                        return Err(Refusal::Malformed(
                            "--x_warning_level requires at least one NAME:SEVERITY value"
                                .to_string(),
                        ));
                    }
                    for value in values {
                        // javac keeps its warnings under `--warn off` when an entry raises a
                        // category back to a warning or an error (`buildJavaOptions`).
                        raises_warnings |= value.ends_with(":error") || value.ends_with(":warning");
                        unit.kotlinc_args.push(format!("-Xwarning-level={value}"));
                    }
                }
                "--x_xlanguage" => {
                    if values.is_empty() {
                        return Err(Refusal::Malformed(
                            "--x_xlanguage requires at least one feature".to_string(),
                        ));
                    }
                    for value in values {
                        translate_language_features(&mut unit, &value, flag)?;
                    }
                }
                "--plugin_options" => {
                    return Err(Refusal::Unsupported(
                        "--plugin_options: krusty does not run compiler plugins supplied by the build"
                            .to_string(),
                    ));
                }
                // `strip_prefix:add_prefix:file` groups. Packaged after compilation.
                "--resources" => {
                    if values.is_empty() {
                        return Err(Refusal::Malformed(
                            "--resources requires at least one strip_prefix:add_prefix:file group"
                                .to_string(),
                        ));
                    }
                    for value in values {
                        unit.resources.push(parse_resource_group(&value)?);
                    }
                }
                // Understood, but they do not change the bytes krusty writes.
                _ => unit.inert.push(flag.to_string()),
            }
            continue;
        }
        if let Some(mapped) = boolean_option(flag) {
            match mapped {
                Some(kotlinc_flag) => unit.kotlinc_args.push(kotlinc_flag.to_string()),
                None => unit.inert.push(flag.to_string()),
            }
            index += 1;
            continue;
        }
        match flag {
            "--out" => {
                unit.output_jar = PathBuf::from(value_of(index, flag)?);
                index += 2;
            }
            "--abi-out" => {
                unit.abi_jar = Some(PathBuf::from(value_of(index, flag)?));
                index += 2;
            }
            "--kotlin-cri-out" => {
                unit.cri_file = Some(PathBuf::from(value_of(index, flag)?));
                index += 2;
            }
            "--kotlin_module_name" => {
                unit.module_name = Some(value_of(index, flag)?);
                index += 2;
            }
            "--target_label" => {
                unit.target_label = Some(value_of(index, flag)?);
                index += 2;
            }
            "--java-count" => {
                let value = value_of(index, flag)?;
                java_count = Some(
                    value
                        .parse()
                        .map_err(|_| Refusal::Malformed(format!("--java-count {value}")))?,
                );
                index += 2;
            }
            "--jvm_target" => {
                let value = value_of(index, flag)?;
                unit.kotlinc_args.push("-jvm-target".to_string());
                unit.kotlinc_args.push(value.clone());
                unit.java.jvm_target = Some(value);
                index += 2;
            }
            "--no-proc" => {
                unit.java.no_proc = true;
                index += 1;
            }
            "--api_version" => {
                let value = value_of(index, flag)?;
                unit.kotlinc_args.push("-api-version".to_owned());
                unit.kotlinc_args.push(value);
                index += 2;
            }
            "--language_version" => {
                let value = value_of(index, flag)?;
                unit.kotlinc_args.push("-language-version".to_owned());
                unit.kotlinc_args.push(value);
                index += 2;
            }
            // The four options this worker exists to honor. Each selects an artifact SHAPE, so a
            // value krusty does not emit is refused rather than compiled as something else.
            "--jvm_default" => {
                let value = value_of(index, flag)?;
                let mode = JvmDefaultMode::parse(&value)
                    .ok_or_else(|| Refusal::Unsupported(format!("--jvm_default {value}")))?;
                unit.kotlinc_args.push("-jvm-default".to_string());
                unit.kotlinc_args.push(
                    match mode {
                        JvmDefaultMode::Enable => "enable",
                        JvmDefaultMode::NoCompatibility => "no-compatibility",
                        JvmDefaultMode::Disable => "disable",
                    }
                    .to_string(),
                );
                index += 2;
            }
            "--x_lambdas" | "--x_sam_conversions" => {
                let value = value_of(index, flag)?;
                // Both strategies are emitted, so the value is forwarded rather than gated. A third
                // value would select a shape that does not exist and is still refused.
                if value != "indy" && value != "class" {
                    return Err(Refusal::Unsupported(format!("{flag} {value}")));
                }
                let spelling = if flag == "--x_lambdas" {
                    "-Xlambdas"
                } else {
                    "-Xsam-conversions"
                };
                unit.kotlinc_args.push(format!("{spelling}={value}"));
                index += 2;
            }
            "--x_no_param_assertions" => {
                unit.kotlinc_args.push("-Xno-param-assertions".to_string());
                index += 1;
            }
            "--x_no_call_assertions" => {
                unit.kotlinc_args.push("-Xno-call-assertions".to_string());
                index += 1;
            }
            // A kotlinc flag forwarded verbatim by the rule (`kotlinc_opts`). Without this the rule
            // would have to re-spell every option in the worker vocabulary, and anything it could
            // not spell would silently not reach the compiler.
            "--kotlinc-arg" => {
                let value = value_of(index, flag)?;
                if is_worker_owned_kotlinc_option(&value) {
                    return Err(Refusal::Unsupported(format!(
                        "{flag} {value}: output, module, sources, and classpath are owned by the work request"
                    )));
                } else if let Some(features) = value.strip_prefix("-XXLanguage:") {
                    translate_language_features(&mut unit, features, flag)?;
                } else {
                    match value.as_str() {
                        "-Xexplicit-api=disable" => unit.inert.push(value),
                        // kotlinc 2.4.10 (JVM) accepts `-Xwasm-kclass-fqn` with only a "flag is
                        // not supported by this version of the compiler" warning and emits
                        // byte-identical output with and without it (measured), so a target
                        // forwarding it (intellij-community's fleet.multiplatform.shims,
                        // fleet.util.codepoints) is safe to build. Inert here for Bazel to print;
                        // deliberately NOT generalized to other `-Xwasm-*` flags.
                        "-Xwasm-kclass-fqn" => unit.inert.push(value),
                        // The common CLI parser validates the typed diagnostic identity and applies
                        // its severity. Keeping the argument here also lets duplicate spellings from
                        // the two worker surfaces be rejected in one place.
                        _ if value.starts_with("-Xwarning-level=") => unit.kotlinc_args.push(value),
                        _ => unit.kotlinc_args.push(value),
                    }
                }
                index += 2;
            }
            "--x_explicit_api" => {
                let value = value_of(index, flag)?;
                if value == "disable" {
                    unit.inert.push(format!("--x_explicit_api {value}"));
                } else {
                    return Err(Refusal::Unsupported(format!("{flag} {value}")));
                }
                index += 2;
            }
            "--warn" => {
                let value = value_of(index, flag)?;
                if value == "off" {
                    unit.kotlinc_args.push("-nowarn".to_string());
                    warn_off = true;
                } else {
                    return Err(Refusal::Unsupported(format!("{flag} {value}")));
                }
                index += 2;
            }
            "--plugin-id" | "--plugin-classpath" => {
                // A compiler plugin changes what is emitted, and krusty does not load bazel-supplied
                // plugin jars. Refusing keeps a plugin's output from silently going missing.
                return Err(Refusal::Unsupported(format!(
                    "{flag}: krusty does not load compiler plugins supplied by the build"
                )));
            }
            // `--reduced-classpath-mode true` (and the `--direct-dependencies` list that accompanies
            // it) narrow what the builder puts on the classpath. Ignoring the optimization is safe:
            // krusty still receives the full `--cp`.
            "--reduced-classpath-mode" => {
                let value = value_of(index, flag)?;
                if !matches!(value.as_str(), "true" | "false") {
                    return Err(Refusal::Malformed(format!(
                        "{flag} {value}: expected true or false"
                    )));
                }
                unit.inert.push(format!("{flag} {value}"));
                index += 2;
            }
            "--x_compiler_plugin_order" => {
                unit.inert.push(flag.to_string());
                index += 1;
                // Plugin order is a list. Consume all entries rather than meeting them as stray
                // arguments; any actual plugin is refused by the plugin flags above.
                while index < arguments.len() && !arguments[index].starts_with("--") {
                    index += 1;
                }
            }
            other if other.starts_with("--") => {
                // An unknown worker option may well select a shape; refusing is the safe default.
                return Err(Refusal::Unsupported(format!(
                    "unknown worker option {other}"
                )));
            }
            other => {
                return Err(Refusal::Malformed(format!("unexpected argument {other}")));
            }
        }
    }

    if !source_jars.is_empty() {
        return Err(Refusal::Unsupported(format!(
            "--src-jars ({}): krusty compiles source files, not source jars",
            source_jars.join(", ")
        )));
    }
    // The builder states how many of `--srcs` are Java. A count that disagrees means the request
    // was assembled from a different source list than the one it carries.
    if let Some(count) = java_count {
        if count != unit.java.sources.len() {
            return Err(Refusal::Malformed(format!(
                "--java-count {count}, but --srcs lists {} Java source(s)",
                unit.java.sources.len()
            )));
        }
    }
    unit.java.no_warn = warn_off && !raises_warnings;
    if unit.output_jar.as_os_str().is_empty() {
        return Err(Refusal::Malformed("no --out".to_string()));
    }
    // `kotlinc_opts` is intentionally open-ended at the Starlark surface. The batch CLI accepts
    // unknown compatibility flags for interactive use, but a worker must be stricter: silently
    // ignoring an unknown target option can emit a different artifact. Everything intentionally
    // inert was removed above; every remaining option must be genuinely modeled by the CLI.
    let parsed = crate::cli::parse(unit.kotlinc_args.clone());
    if !parsed.argument_errors.is_empty() {
        return Err(Refusal::Unsupported(parsed.argument_errors.join("; ")));
    }
    if !parsed.errors.is_empty() {
        return Err(Refusal::Unsupported(parsed.errors.join("; ")));
    }
    let feature_errors: Vec<&str> = parsed
        .language_feature_problems
        .iter()
        .filter(|problem| problem.is_error)
        .map(|problem| problem.message.as_str())
        .chain(parsed.configuration_errors.iter().map(String::as_str))
        .collect();
    if !feature_errors.is_empty() {
        return Err(Refusal::Unsupported(feature_errors.join("; ")));
    }
    if !parsed.ignored.is_empty() {
        return Err(Refusal::Unsupported(format!(
            "compiler option(s) not modeled by krusty: {}",
            parsed.ignored.join(", ")
        )));
    }
    // A warn-and-ignore flag (`-Xwasm-kclass-fqn`) must be intercepted as an INERT value above so
    // Bazel sees the no-effect note. Other syntax warnings can belong to fully modeled arguments —
    // a deprecated alias or repeated value, for example — and must reach the response rather than
    // being misclassified as missing semantics.
    if !parsed.unknown_extra_flags.is_empty() {
        return Err(Refusal::Unsupported(format!(
            "warn-and-ignore option(s) missing a worker inert arm: {}",
            parsed.unknown_extra_flags.join(", ")
        )));
    }
    if !parsed.sources.is_empty() {
        return Err(Refusal::Malformed(format!(
            "unexpected positional kotlinc option value(s): {}",
            parsed.sources.join(", ")
        )));
    }
    unit.argument_warnings = parsed.argument_warnings;
    Ok(unit)
}

/// Expand `--flagfile=<path>` entries (one argument per line), which is how bazel passes a long
/// argument list to this worker.
pub fn expand_flagfiles(arguments: &[String]) -> std::io::Result<Vec<String>> {
    let mut expanded = Vec::new();
    for argument in arguments {
        match argument.strip_prefix("--flagfile=") {
            Some(path) => {
                for line in std::fs::read_to_string(path)?.lines() {
                    if !line.is_empty() {
                        expanded.push(line.to_string());
                    }
                }
            }
            None => expanded.push(argument.clone()),
        }
    }
    Ok(expanded)
}

fn parse_resource_group(value: &str) -> Result<ResourceGroup, Refusal> {
    let mut parts = value.split(':');
    let strip_prefix = parts.next().unwrap_or("");
    let Some(add_prefix) = parts.next() else {
        return Err(Refusal::Malformed(format!(
            "--resources {value}: expected strip_prefix:add_prefix:file"
        )));
    };
    let files: Vec<PathBuf> = parts.map(PathBuf::from).collect();
    if files.is_empty() || files.iter().any(|file| file.as_os_str().is_empty()) {
        return Err(Refusal::Malformed(format!(
            "--resources {value}: expected strip_prefix:add_prefix:file"
        )));
    }
    Ok(ResourceGroup {
        strip_prefix: strip_prefix.to_string(),
        add_prefix: add_prefix.to_string(),
        files,
    })
}

/// Jar entry for one resource file. `strip_prefix` is removed and `add_prefix` is prepended, which
/// is how `jvm_library` names entries in the module jar.
pub fn resource_entry_name(
    strip_prefix: &str,
    add_prefix: &str,
    path: &std::path::Path,
) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or_else(|| "resource path is not UTF-8".to_string())?
        .replace('\\', "/");
    let relative = if strip_prefix.is_empty() {
        path.strip_prefix("./").unwrap_or(&path).to_string()
    } else {
        let normalized_prefix = strip_prefix.replace('\\', "/");
        let prefix = normalized_prefix.trim_end_matches('/');
        let rest = path
            .strip_prefix(prefix)
            .ok_or_else(|| format!("resource {path} is outside strip prefix {prefix}"))?;
        let rest = rest
            .strip_prefix('/')
            .filter(|rest| !rest.is_empty())
            .ok_or_else(|| format!("resource {path} is outside strip prefix {prefix}"))?;
        rest.to_string()
    };
    let add = add_prefix.replace('\\', "/");
    let entry = if add.is_empty() {
        relative
    } else {
        format!("{add}/{relative}")
    };
    let has_drive_prefix = entry
        .as_bytes()
        .get(..2)
        .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':');
    if entry.is_empty()
        || entry.starts_with('/')
        || has_drive_prefix
        || entry.chars().any(char::is_control)
        || entry
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(format!(
            "resource entry {entry:?} is not a canonical relative jar path"
        ));
    }
    Ok(entry)
}

/// Copy `--resources` into an existing jar. Entry names follow [`resource_entry_name`].
pub fn package_resources(jar: &std::path::Path, groups: &[ResourceGroup]) -> Result<(), String> {
    let mut planned: Vec<(String, PathBuf)> = Vec::new();
    for group in groups {
        for listed in &group.files {
            let name = resource_entry_name(&group.strip_prefix, &group.add_prefix, listed)?;
            planned.push((name, listed.clone()));
        }
    }
    append_entries(jar, planned)
}

/// Append files to an existing jar, each under its entry name, in order. An entry name the jar or
/// an earlier file already holds fails before the jar is changed.
fn append_entries(jar: &std::path::Path, planned: Vec<(String, PathBuf)>) -> Result<(), String> {
    use std::collections::BTreeSet;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    if planned.is_empty() {
        return Ok(());
    }

    let existing = {
        let file = std::fs::File::open(jar)
            .map_err(|error| format!("cannot open {}: {error}", jar.display()))?;
        let archive = krusty::archive::open_file(file)
            .map_err(|error| format!("cannot read {}: {error}", jar.display()))?;
        archive
            .file_names()
            .map(|name| name.map(Cow::into_owned))
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|error| format!("cannot read {}: {error}", jar.display()))?
    };
    let mut seen = existing;
    for (name, _) in &planned {
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate jar entry {name}"));
        }
    }

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(jar)
        .map_err(|error| format!("cannot open {}: {error}", jar.display()))?;
    let mut writer = zip::ZipWriter::new_append(file)
        .map_err(|error| format!("cannot append to {}: {error}", jar.display()))?;
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, path) in planned {
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        writer
            .start_file(name, options)
            .map_err(|error| format!("cannot add to {}: {error}", jar.display()))?;
        writer
            .write_all(&bytes)
            .map_err(|error| format!("cannot add to {}: {error}", jar.display()))?;
    }
    writer
        .finish()
        .map_err(|error| format!("cannot finish {}: {error}", jar.display()))?;
    Ok(())
}

/// One request of bazel's JSON worker protocol. Only the fields this worker reads are modelled;
/// bazel is free to send more.
#[derive(Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRequest {
    #[serde(default)]
    pub arguments: Vec<String>,
    /// The action's inputs with their digests, which a handoff forwards to the JPS builder.
    #[serde(default)]
    pub inputs: Vec<WorkInput>,
    #[serde(default)]
    pub request_id: i32,
    #[serde(default)]
    pub cancel: bool,
}

/// One input of a work request: its path and its digest, base64 in the JSON protocol.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct WorkInput {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub digest: String,
}

/// What the worker's own command line configures for every request it serves: kotlinc arguments
/// the build adds to each target's (`--kotlinc-arg`), and the JPS builder a target krusty does not
/// compile is handed to (`--jps-builder`, one argument of its command at a time).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct WorkerSetup {
    pub kotlinc_args: Vec<String>,
    pub handoff: Option<Handoff>,
}

impl WorkerSetup {
    /// Read the worker's startup arguments; bazel appends `--persistent_worker` to them.
    pub fn from_startup(arguments: &[String]) -> Result<Self, String> {
        let mut setup = Self::default();
        let mut builder = Vec::new();
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{argument} needs a value"))
            };
            match argument.as_str() {
                "--persistent_worker" => {}
                "--kotlinc-arg" => setup.kotlinc_args.push(value()?),
                "--jps-builder" => builder.push(value()?),
                other => return Err(format!("unknown worker startup argument `{other}`")),
            }
        }
        if !builder.is_empty() {
            setup.handoff = Some(Handoff::new(builder)?);
        }
        Ok(setup)
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkResponse {
    pub exit_code: i32,
    pub output: String,
    pub request_id: i32,
}

/// Serve one request. Returns the response bazel should receive. A request krusty does not
/// compile is handed to the JPS builder when `setup` names one, and fails otherwise.
pub fn serve(
    request: &WorkRequest,
    setup: &WorkerSetup,
    compile: &dyn Fn(WorkUnit) -> Result<(), WorkFailure>,
) -> WorkResponse {
    let arguments = match expand_flagfiles(&request.arguments) {
        Ok(arguments) => arguments,
        Err(error) => {
            return WorkResponse {
                exit_code: 1,
                output: format!("krusty: cannot read flagfile: {error}"),
                request_id: request.request_id,
            }
        }
    };
    let (note, label, failure) = match translate(&arguments) {
        Ok(mut unit) => {
            // Report what was understood but had no effect. `WorkResponse::output` is the channel
            // bazel prints, and silently dropping an option the target set is the habit this module
            // exists to avoid.
            let mut note = unit
                .argument_warnings
                .iter()
                .map(|warning| format!("warning: {}\n", crate::kotlinc_arguments::render(warning)))
                .collect::<String>();
            if !unit.inert.is_empty() {
                note.push_str(&format!(
                    "krusty: no effect on output: {}\n",
                    unit.inert.join(" ")
                ));
            }
            // The build's own arguments come first, so a target's kotlinc options override them.
            unit.kotlinc_args
                .splice(0..0, setup.kotlinc_args.iter().cloned());
            let label = unit.target_label.clone();
            match compile(unit) {
                Ok(()) => {
                    return WorkResponse {
                        exit_code: 0,
                        output: note,
                        request_id: request.request_id,
                    }
                }
                Err(failure) => (note, label, failure),
            }
        }
        Err(refusal) => (
            String::new(),
            None,
            WorkFailure::new(FailureKind::CommandLine, format!("krusty: {refusal}")),
        ),
    };
    match &setup.handoff {
        Some(handoff) => handoff.compile(request, label.as_deref(), &failure, &note),
        None => WorkResponse {
            exit_code: 1,
            output: format!("{note}{}", failure.report),
            request_id: request.request_id,
        },
    }
}

/// Read work requests until stdin closes, one JSON object per line.
pub fn run(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    setup: &WorkerSetup,
    compile: &dyn Fn(WorkUnit) -> Result<(), WorkFailure>,
) -> std::io::Result<()> {
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<WorkRequest>(&line) {
            // A cancellation for a request already finished is answered, not compiled: this worker
            // serves one request at a time, so there is never work in flight to stop.
            Ok(request) if request.cancel => WorkResponse {
                exit_code: 0,
                output: String::new(),
                request_id: request.request_id,
            },
            Ok(request) => serve(&request, setup, compile),
            Err(error) => WorkResponse {
                exit_code: 1,
                output: format!("krusty: unreadable work request: {error}"),
                request_id: 0,
            },
        };
        writeln!(
            output,
            "{}",
            serde_json::to_string(&response).expect("render response")
        )?;
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn the_compiler_reads_every_classpath_entry_of_a_work_unit() {
        let unit = translate(&args(&[
            "--srcs",
            "A.kt",
            "--cp",
            "lib/a.jar",
            "lib/b.jar",
            "--out",
            "o.jar",
        ]))
        .expect("must translate");
        let parsed = crate::cli::parse(unit.compiler_arguments());
        assert_eq!(
            parsed.classpath,
            vec![PathBuf::from("lib/a.jar"), PathBuf::from("lib/b.jar")]
        );
    }

    /// intellij-community's integration patch forwards the build's own Kotlin release, so the worker
    /// reproduces the kotlinc the project's builder embeds, and refuses a release krusty lacks.
    #[test]
    fn a_forwarded_reference_version_selects_the_kotlinc_release() {
        use krusty::kotlin_version::KotlinVersion;
        let request = |version: &str| {
            translate(&args(&[
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
                "--kotlinc-arg",
                &format!("-Xkotlin-reference-version={version}"),
            ]))
        };
        let unit = request("2.4.21").expect("must translate");
        let parsed = crate::cli::parse(unit.compiler_arguments());
        assert_eq!(
            parsed.kotlin_reference_version,
            Some(KotlinVersion::V2_4_21)
        );
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let supported = KotlinVersion::supported()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(
            request("2.4.10-RC2"),
            Err(Refusal::Unsupported(format!(
                "-Xkotlin-reference-version=2.4.10-RC2 names no supported Kotlin release \
                 (supported: {supported})"
            )))
        );
    }

    /// The argument list intellij-community's own rules produce for a Kotlin-only target.
    fn intellij_request() -> Vec<String> {
        args(&[
            "--target_label",
            "//platform/util:util",
            "--kotlin_module_name",
            "intellij.platform.util",
            "--jvm_target",
            "25",
            "--api_version",
            "2.4",
            "--language_version",
            "2.4",
            "--jvm_default",
            "no-compatibility",
            "--x_lambdas",
            "indy",
            "--x_sam_conversions",
            "indy",
            "--progressive",
            "--warn",
            "off",
            "--x_xlanguage",
            "+AllowEagerSupertypeAccessibilityChecks",
            "--srcs",
            "a/A.kt",
            "a/B.kt",
            "--cp",
            "lib/dep.jar",
            "--out",
            "out/util.jar",
            "--abi-out",
            "out/util.abi.jar",
            "--kotlin-cri-out",
            "out/util.kotlinCriStorage",
            "--java-count",
            "0",
        ])
    }

    #[test]
    fn the_real_argument_surface_translates() {
        let unit = translate(&intellij_request()).expect("must translate");
        assert_eq!(unit.output_jar, PathBuf::from("out/util.jar"));
        assert_eq!(unit.abi_jar, Some(PathBuf::from("out/util.abi.jar")));
        assert_eq!(
            unit.cri_file,
            Some(PathBuf::from("out/util.kotlinCriStorage"))
        );
        assert_eq!(unit.module_name.as_deref(), Some("intellij.platform.util"));
        assert_eq!(
            unit.sources,
            vec![PathBuf::from("a/A.kt"), PathBuf::from("a/B.kt")]
        );
        assert_eq!(unit.classpath, vec![PathBuf::from("lib/dep.jar")]);
        let parsed = crate::cli::parse(unit.kotlinc_args.clone());
        assert_eq!(parsed.jvm_default, JvmDefaultMode::NoCompatibility);
        assert_eq!(
            parsed.language_settings.language_version,
            krusty::language_version::LanguageVersion::V2_4
        );
        assert_eq!(
            parsed.language_settings.api_version,
            krusty::language_version::LanguageVersion::V2_4
        );
        assert!(!parsed.no_param_assertions);
        for expected in [
            "-jvm-target",
            "25",
            "-api-version",
            "2.4",
            "-language-version",
            "-progressive",
        ] {
            assert!(
                unit.kotlinc_args.iter().any(|a| a == expected),
                "{expected} missing from {:?}",
                unit.kotlinc_args
            );
        }
        assert_eq!(
            parsed.warning_policy.compiler_disposition(None),
            crate::cli::WarningDisposition::Disabled
        );
    }

    /// The worker and batch surfaces must not disagree about a semantic option: every progressive
    /// feature is modeled, so the rule-owned spelling and a forwarded kotlinc spelling both reach
    /// the shared compiler.
    #[test]
    fn progressive_is_forwarded_through_every_worker_surface() {
        for (progressive, forwarded) in [
            (vec!["--progressive"], "-progressive"),
            (vec!["--kotlinc-arg", "-progressive"], "-progressive"),
            (vec!["--kotlinc-arg", "-Xprogressive"], "-Xprogressive"),
        ] {
            let mut request = progressive;
            request.extend(["--srcs", "A.kt", "--out", "o.jar"]);
            let unit = translate(&args(&request)).expect("progressive is modeled");
            assert_eq!(unit.kotlinc_args, [forwarded.to_string()]);
        }
    }

    #[test]
    fn standard_language_and_api_versions_are_forwarded_to_the_shared_cli() {
        let unit = translate(&args(&[
            "--language_version",
            "2.2",
            "--api_version",
            "2.2",
            "--srcs",
            "A.kt",
            "--out",
            "out.jar",
        ]))
        .expect("standard version settings translate");
        assert!(unit.inert.is_empty(), "{:?}", unit.inert);
        assert_eq!(unit.sources, vec![PathBuf::from("A.kt")]);
        assert_eq!(unit.output_jar, PathBuf::from("out.jar"));
        // Sources and the output jar stay on the work unit. `kotlinc_args` is only the flags the
        // shared CLI parses; appending the source here would make the worker refuse the request
        // as an unexpected positional value.
        assert_eq!(
            unit.kotlinc_args,
            args(&["-language-version", "2.2", "-api-version", "2.2"])
        );
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        assert_eq!(
            parsed.language_settings.language_version,
            krusty::language_version::LanguageVersion::V2_2
        );
        assert_eq!(
            parsed.language_settings.api_version,
            krusty::language_version::LanguageVersion::V2_2
        );
    }

    /// `-Xjvm-default=all` is what the project builds with, and the worker spells it
    /// `--jvm_default no-compatibility`.
    #[test]
    fn the_projects_jvm_default_selects_no_compatibility() {
        let unit = translate(&args(&[
            "--jvm_default",
            "no-compatibility",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap();
        assert_eq!(
            crate::cli::parse(unit.kotlinc_args).jvm_default,
            JvmDefaultMode::NoCompatibility
        );
    }

    /// All three of kotlinc's `-jvm-default` strategies are emitted, so a target pinning any of them
    /// translates. A value naming none of them is still refused rather than compiled as some other
    /// interface shape.
    #[test]
    fn every_jvm_default_mode_translates_and_nonsense_is_refused() {
        for mode in ["disable", "enable", "no-compatibility"] {
            let unit = translate(&args(&[
                "--jvm_default",
                mode,
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]))
            .unwrap_or_else(|error| panic!("{mode} must translate: {error:?}"));
            assert_eq!(unit.sources, vec![PathBuf::from("A.kt")]);
        }
        let refusal = translate(&args(&[
            "--jvm_default",
            "sideways",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
    }

    #[test]
    fn the_assertion_flags_are_honored() {
        let unit = translate(&args(&[
            "--x_no_param_assertions",
            "--x_no_call_assertions",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap();
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.no_param_assertions);
        assert!(parsed.no_call_assertions);
    }

    /// The class-based lambda strategy is a different class set, so it is refused like any other
    /// shape krusty cannot emit.
    #[test]
    fn each_lambda_strategy_reaches_the_compiler() {
        for (flag, spelling) in [
            ("--x_lambdas", "-Xlambdas"),
            ("--x_sam_conversions", "-Xsam-conversions"),
        ] {
            for value in ["indy", "class"] {
                let unit = translate(&args(&[flag, value, "--srcs", "A.kt", "--out", "o.jar"]))
                    .unwrap_or_else(|refusal| panic!("{flag} {value}: {refusal:?}"));
                assert!(
                    unit.kotlinc_args.contains(&format!("{spelling}={value}")),
                    "{flag} {value} did not reach the compiler: {:?}",
                    unit.kotlinc_args
                );
            }
            // A value naming no strategy would otherwise compile as one of them.
            let refusal = translate(&args(&[
                flag, "nonesuch", "--srcs", "A.kt", "--out", "o.jar",
            ]))
            .unwrap_err();
            assert!(
                matches!(refusal, Refusal::Unsupported(_)),
                "{flag}: {refusal:?}"
            );
        }
    }

    /// A mixed target compiles as the builder compiles it: the `.java` sources reach krusty as
    /// declarations, after the Kotlin sources, and javac gets the options the request selects.
    #[test]
    fn java_sources_are_declarations_for_krusty_and_inputs_for_javac() {
        let unit = translate(&args(&[
            "--jvm_target",
            "17",
            "--warn",
            "off",
            "--no-proc",
            "--add-export",
            "java.desktop/sun.awt=ALL-UNNAMED",
            "--srcs",
            "B.java",
            "A.kt",
            "--out",
            "o.jar",
            "--java-count",
            "1",
        ]))
        .expect("must translate");
        assert_eq!(unit.sources, [PathBuf::from("A.kt")]);
        assert_eq!(
            unit.java,
            JavaCompilation {
                sources: vec![PathBuf::from("B.java")],
                jvm_target: Some("17".to_string()),
                no_warn: true,
                no_proc: true,
                add_exports: vec!["java.desktop/sun.awt=ALL-UNNAMED".to_string()],
            }
        );
        assert_eq!(
            unit.compiler_arguments(),
            args(&[
                "-d",
                "o.jar",
                "-jvm-target",
                "17",
                "-nowarn",
                "A.kt",
                "B.java"
            ])
        );
        assert!(unit.inert.is_empty(), "{:?}", unit.inert);
    }

    /// `--warn off` keeps javac's warnings when a `--x_warning_level` entry raises a category back
    /// to a warning or an error, as `buildJavaOptions` decides.
    #[test]
    fn a_raised_warning_level_keeps_javac_warnings() {
        let unit = translate(&args(&[
            "--warn",
            "off",
            "--x_warning_level",
            "DEPRECATION:warning",
            "--srcs",
            "B.java",
            "--out",
            "o.jar",
        ]))
        .expect("must translate");
        assert!(!unit.java.no_warn);
    }

    /// A `--java-count` that disagrees with `--srcs` means the request lost or gained a source.
    #[test]
    fn the_java_count_must_match_the_java_sources() {
        assert_eq!(
            translate(&args(&[
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
                "--java-count",
                "3"
            ])),
            Err(Refusal::Malformed(
                "--java-count 3, but --srcs lists 0 Java source(s)".to_string()
            ))
        );
    }

    /// A module declaration needs javac's module path, which the worker does not split.
    #[test]
    fn a_java_module_declaration_is_refused() {
        assert_eq!(
            translate(&args(&["--srcs", "A.kt", "src/module-info.java", "--out", "o.jar"])),
            Err(Refusal::Unsupported(
                "--srcs src/module-info.java: Java module declarations are not compiled by the worker"
                    .to_string()
            ))
        );
    }

    /// The batch CLI silently ignores unknown path kinds. A worker cannot inherit that
    /// compatibility behavior: Bazel would receive a successful jar missing a declared source. A
    /// script is refused as well, since the action does not describe its script definition.
    #[test]
    fn every_declared_source_must_be_batch_compilable_kotlin() {
        for source in ["A.kts", "README", "generated/source.txt"] {
            let refusal = translate(&args(&["--srcs", source, "--out", "o.jar"])).unwrap_err();
            assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
        }
    }

    #[test]
    fn source_jars_and_plugins_are_refused() {
        for arguments in [
            args(&[
                "--srcs",
                "A.kt",
                "--src-jars",
                "gen.srcjar",
                "--out",
                "o.jar",
            ]),
            args(&["--srcs", "A.kt", "--plugin-id", "compose", "--out", "o.jar"]),
        ] {
            assert!(
                matches!(translate(&arguments).unwrap_err(), Refusal::Unsupported(_)),
                "{arguments:?}"
            );
        }
    }

    /// `--resources` and `--reduced-classpath-mode` carry values in the real rules
    /// (`builder-args.bzl:33-40`), so treating either as a bare flag met its value as a stray
    /// argument and failed every target that sets them.
    #[test]
    fn the_value_carrying_structural_flags_are_consumed_whole() {
        let unit = translate(&args(&[
            "--reduced-classpath-mode",
            "true",
            "--direct-dependencies",
            "a.jar",
            "b.jar",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("must translate");
        assert_eq!(unit.sources, vec![PathBuf::from("A.kt")]);

        let malformed = translate(&args(&[
            "--reduced-classpath-mode",
            "true",
            "not-a-second-value",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(malformed, Refusal::Malformed(_)), "{malformed:?}");
    }

    /// intellij-community's module jar names a resource by stripping the resource root. The worker
    /// argument is `strip_prefix:add_prefix:file`, and an empty add prefix is two adjacent colons
    /// (`platform/icons-api/resources::…/intellij.platform.icons.api.xml`).
    #[test]
    fn resources_keep_the_module_jar_entry_name() {
        let unit = translate(&args(&[
            "--resources",
            "platform/icons-api/resources::platform/icons-api/resources/intellij.platform.icons.api.xml",
            "res:com/example:res/a.txt:res/b.txt",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("resources must translate");
        assert_eq!(
            unit.resources,
            vec![
                ResourceGroup {
                    strip_prefix: "platform/icons-api/resources".to_string(),
                    add_prefix: String::new(),
                    files: vec![PathBuf::from(
                        "platform/icons-api/resources/intellij.platform.icons.api.xml"
                    )],
                },
                ResourceGroup {
                    strip_prefix: "res".to_string(),
                    add_prefix: "com/example".to_string(),
                    files: vec![PathBuf::from("res/a.txt"), PathBuf::from("res/b.txt")],
                },
            ]
        );
        assert_eq!(
            resource_entry_name(
                &unit.resources[0].strip_prefix,
                &unit.resources[0].add_prefix,
                &unit.resources[0].files[0],
            )
            .expect("entry"),
            "intellij.platform.icons.api.xml"
        );
        assert_eq!(
            resource_entry_name("res", "com/example", std::path::Path::new("res/a.txt"))
                .expect("entry"),
            "com/example/a.txt"
        );

        for value in ["res", "res:prefix", "res:prefix:"] {
            let refusal = translate(&args(&["--resources", value, "--out", "o.jar"])).unwrap_err();
            assert_eq!(
                refusal,
                Refusal::Malformed(format!(
                    "--resources {value}: expected strip_prefix:add_prefix:file"
                ))
            );
        }
        let missing = translate(&args(&["--resources", "--out", "o.jar"])).unwrap_err();
        assert_eq!(
            missing,
            Refusal::Malformed(
                "--resources requires at least one strip_prefix:add_prefix:file group".to_string()
            )
        );
        let outside =
            resource_entry_name("res", "", std::path::Path::new("other/a.txt")).unwrap_err();
        assert_eq!(outside, "resource other/a.txt is outside strip prefix res");

        for (add, path, entry) in [
            ("../outside", "res/a.txt", "../outside/a.txt"),
            ("/absolute", "res/a.txt", "/absolute/a.txt"),
            ("\\absolute", "res/a.txt", "/absolute/a.txt"),
            ("C:", "res/a.txt", "C:/a.txt"),
            ("com/./example", "res/a.txt", "com/./example/a.txt"),
            ("com//example", "res/a.txt", "com//example/a.txt"),
            ("bad\0name", "res/a.txt", "bad\0name/a.txt"),
        ] {
            assert_eq!(
                resource_entry_name("res", add, std::path::Path::new(path)).unwrap_err(),
                format!("resource entry {entry:?} is not a canonical relative jar path")
            );
        }
        assert_eq!(
            resource_entry_name("", "", std::path::Path::new("../a.txt")).unwrap_err(),
            "resource entry \"../a.txt\" is not a canonical relative jar path"
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let path = PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0xff]));
            assert_eq!(
                resource_entry_name("", "", &path).unwrap_err(),
                "resource path is not UTF-8"
            );
        }
    }

    #[test]
    fn packaged_resources_land_in_the_jar() {
        let dir = std::env::temp_dir().join(format!("krusty-resources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("resources")).unwrap();
        std::fs::write(dir.join("resources/mod.xml"), b"<module/>").unwrap();
        let jar = dir.join("out.jar");
        {
            use zip::write::SimpleFileOptions;
            let file = std::fs::File::create(&jar).expect("create jar");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("META-INF/MANIFEST.MF", SimpleFileOptions::default())
                .expect("manifest entry");
            std::io::Write::write_all(&mut writer, b"Manifest-Version: 1.0\r\n\r\n")
                .expect("manifest bytes");
            writer.finish().expect("finish jar");
        }
        package_resources(
            &jar,
            &[ResourceGroup {
                strip_prefix: dir.join("resources").display().to_string(),
                add_prefix: String::new(),
                files: vec![dir.join("resources/mod.xml")],
            }],
        )
        .expect("package");

        let mut archive = zip::ZipArchive::new(std::fs::File::open(&jar).unwrap()).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("mod.xml").unwrap(), &mut xml).unwrap();
        assert_eq!(xml, "<module/>");
        assert!(archive.by_name("META-INF/MANIFEST.MF").is_ok());

        let duplicate = package_resources(
            &jar,
            &[ResourceGroup {
                strip_prefix: dir.join("resources").display().to_string(),
                add_prefix: String::new(),
                files: vec![dir.join("resources/mod.xml")],
            }],
        )
        .unwrap_err();
        assert_eq!(duplicate, "duplicate jar entry mod.xml");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rule forwards a target's `kotlinc_opts` as `--kotlinc-arg` each. Without a passthrough
    /// they reached the compiler in non-worker mode only, so flipping `use_worker` silently changed
    /// the emitted artifact — `-Xjvm-default=all` would stop applying and the jar would grow the
    /// `$DefaultImpls` class the target asked not to have.
    #[test]
    fn forwarded_kotlinc_flags_reach_the_compiler() {
        let unit = translate(&args(&[
            "--kotlinc-arg",
            "-Xjvm-default=all",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("must translate");
        assert_eq!(unit.kotlinc_args, vec!["-Xjvm-default=all".to_string()]);
        assert!(unit.inert.is_empty(), "{:?}", unit.inert);
    }

    /// intellij-community's fleet.multiplatform.shims and fleet.util.codepoints targets forward
    /// `-Xwasm-kclass-fqn` through `kotlinc_opts`. kotlinc 2.4.10 (JVM) accepts it with a "flag is
    /// not supported by this version of the compiler" warning and emits byte-identical output with
    /// and without it (measured), so the worker must accept the request and report the flag as
    /// inert rather than refuse it.
    #[test]
    fn wasm_kclass_fqn_is_accepted_as_inert() {
        let unit = translate(&args(&[
            "--kotlinc-arg",
            "-Xwasm-kclass-fqn",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("a measured-inert kotlinc flag must translate");
        assert_eq!(unit.inert, vec!["-Xwasm-kclass-fqn".to_string()]);
        assert!(
            unit.kotlinc_args.is_empty(),
            "the flag changes nothing, so nothing is forwarded: {:?}",
            unit.kotlinc_args
        );
    }

    /// Both worker spellings normalize to the standard CLI option. Validation and policy
    /// application then happen through the same typed diagnostic registry as a batch invocation.
    #[test]
    fn warning_level_is_forwarded_to_the_typed_cli_policy() {
        let unit = translate(&args(&[
            "--kotlinc-arg",
            "-Xwarning-level=REDUNDANT_CLI_ARG:disabled",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("a valid warning level must translate");
        assert!(unit.inert.is_empty(), "{:?}", unit.inert);
        assert_eq!(
            unit.kotlinc_args,
            vec!["-Xwarning-level=REDUNDANT_CLI_ARG:disabled".to_string()]
        );

        let unit = translate(&args(&[
            "--x_warning_level",
            "REDUNDANT_CLI_ARG:disabled",
            "DEPRECATED_LANGUAGE_VERSION:error",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("the worker's own warning-level flag must translate");
        assert_eq!(
            unit.kotlinc_args,
            vec![
                "-Xwarning-level=REDUNDANT_CLI_ARG:disabled".to_string(),
                "-Xwarning-level=DEPRECATED_LANGUAGE_VERSION:error".to_string(),
            ]
        );
        assert!(unit.inert.is_empty(), "{:?}", unit.inert);

        for arguments in [
            args(&[
                "--kotlinc-arg",
                "-Xwarning-level=REDUNDANT_CLI_ARG:loud",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
            args(&[
                "--x_warning_level",
                "REDUNDANT_CLI_ARG",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
        ] {
            let refusal = translate(&arguments).unwrap_err();
            assert!(
                matches!(refusal, Refusal::Unsupported(_)),
                "{arguments:?}: {refusal:?}"
            );
        }

        let refusal = translate(&args(&[
            "--x_warning_level",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert_eq!(
            refusal,
            Refusal::Malformed(
                "--x_warning_level requires at least one NAME:SEVERITY value".to_string()
            )
        );
    }

    /// A target-provided compiler flag is safe only when the CLI actually models it. Accepting an
    /// arbitrary `-X...` here lets the CLI's compatibility parser ignore a potentially
    /// output-changing option while the worker reports success.
    #[test]
    fn an_unknown_forwarded_kotlinc_flag_is_refused() {
        for option in [
            "-Xshape-krusty-does-not-model",
            "-XXLanguage:+ShapeKrustyDoesNotModel",
        ] {
            let refusal = translate(&args(&[
                "--kotlinc-arg",
                option,
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]))
            .unwrap_err();
            assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
        }
    }

    #[test]
    fn forwarded_flags_cannot_replace_worker_owned_inputs_or_outputs() {
        for option in [
            "-d",
            "-d=elsewhere.jar",
            "-cp",
            "-cp=other.jar",
            "-classpath",
            "-classpath=other.jar",
            "-class-path",
            "-class-path=other.jar",
            "-module-name",
            "-module-name=other",
            "@other.args",
        ] {
            let refusal = translate(&args(&[
                "--kotlinc-arg",
                option,
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]))
            .unwrap_err();
            assert_eq!(
                refusal,
                Refusal::Unsupported(format!(
                    "--kotlinc-arg {option}: output, module, sources, and classpath are owned by the work request"
                ))
            );
        }
    }

    /// `--x_consistent_data_class_copy_visibility` translates to the kotlinc flag AND the CLI now
    /// genuinely models it, so the worker's strict `ignored`-must-be-empty gate accepts the request
    /// (this option previously blocked the intellij fleet.* modules).
    #[test]
    fn consistent_copy_visibility_is_accepted() {
        let unit = translate(&args(&[
            "--x_consistent_data_class_copy_visibility",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("the option must be accepted, not refused");
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed
            .language_settings
            .features
            .has("DataClassCopyRespectsConstructorVisibility"));
    }

    #[test]
    fn explicit_backing_fields_are_accepted() {
        let unit = translate(&args(&[
            "--x_explicit_backing_fields",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("explicit backing fields must be accepted");
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed
            .language_settings
            .features
            .has("ExplicitBackingFields"));
    }

    #[test]
    fn context_parameters_are_accepted() {
        let unit = translate(&args(&[
            "--x_context_parameters",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("context parameters must be accepted");
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed.language_settings.features.has("ContextParameters"));
    }

    #[test]
    fn modeled_name_based_destructuring_mode_is_accepted() {
        let unit = translate(&args(&[
            "--kotlinc-arg",
            "-Xname-based-destructuring=only-syntax",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("name-based destructuring must be accepted");
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert!(parsed
            .language_settings
            .features
            .has("NameBasedDestructuring"));
        assert!(!parsed
            .language_settings
            .features
            .has("EnableNameBasedDestructuringShortForm"));
    }

    #[test]
    fn modeled_language_features_reach_the_frontend() {
        let unit = translate(&args(&[
            "--x_xlanguage",
            "+WhenGuards",
            "-ContextParameters",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("modeled language features");
        let parsed = crate::cli::parse(unit.kotlinc_args);
        assert!(parsed.language_settings.features.has("WhenGuards"));
        assert!(!parsed.language_settings.features.has("ContextParameters"));
    }

    #[test]
    fn unknown_worker_language_features_are_refused() {
        let refusal = translate(&args(&[
            "--x_xlanguage",
            "+ShapeKrustyDoesNotModel",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");

        for arguments in [
            args(&["--x_xlanguage", ",", "--srcs", "A.kt", "--out", "o.jar"]),
            args(&[
                "--kotlinc-arg",
                "-XXLanguage:",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
        ] {
            assert!(
                matches!(translate(&arguments).unwrap_err(), Refusal::Malformed(_)),
                "{arguments:?}"
            );
        }
    }

    /// Plugin options are output-affecting even if a malformed request omitted the companion
    /// plugin id/classpath flags. They must never fall through the generic inert-list path.
    #[test]
    fn plugin_options_are_refused() {
        let refusal = translate(&args(&[
            "--plugin_options",
            "plugin:option=value",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
    }

    #[test]
    fn associates_are_preserved_as_friend_paths() {
        let unit = translate(&args(&[
            "--friends",
            "peer.jar",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("friend path should translate");
        assert_eq!(unit.friend_paths, [PathBuf::from("peer.jar")]);
    }

    /// Options understood but without effect are REPORTED, through the channel bazel prints.
    #[test]
    fn inert_options_are_reported_in_the_response() {
        let request = WorkRequest {
            arguments: args(&[
                "--x_explicit_api",
                "disable",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
            inputs: Vec::new(),
            request_id: 4,
            cancel: false,
        };
        let response = serve(&request, &WorkerSetup::default(), &|_unit| Ok(()));
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output: "krusty: no effect on output: --x_explicit_api disable\n".to_string(),
                request_id: 4,
            }
        );
    }

    #[test]
    fn modeled_argument_warnings_are_reported_in_the_response() {
        let request = WorkRequest {
            arguments: args(&[
                "--kotlinc-arg",
                "-Xopt-in=marker.Experimental",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
            inputs: Vec::new(),
            request_id: 5,
            cancel: false,
        };
        let response = serve(&request, &WorkerSetup::default(), &|_unit| Ok(()));
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output: "warning: argument -Xopt-in is deprecated. Please use -opt-in instead\n"
                    .to_string(),
                request_id: 5,
            }
        );
    }

    #[test]
    fn repeated_modeled_arguments_warn_without_becoming_unhandled() {
        let request = WorkRequest {
            arguments: args(&[
                "--kotlinc-arg",
                "-Xlambdas=class",
                "--kotlinc-arg",
                "-Xlambdas=indy",
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
            ]),
            inputs: Vec::new(),
            request_id: 6,
            cancel: false,
        };
        let response = serve(&request, &WorkerSetup::default(), &|_unit| Ok(()));
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output: "warning: argument '-Xlambdas' is passed multiple times: 'class', 'indy'. The last value will be used.\n"
                    .to_string(),
                request_id: 6,
            }
        );
    }

    #[test]
    fn a_request_without_an_output_is_malformed() {
        assert!(matches!(
            translate(&args(&["--srcs", "A.kt"])).unwrap_err(),
            Refusal::Malformed(_)
        ));
        assert!(matches!(
            translate(&args(&["--out"])).unwrap_err(),
            Refusal::Malformed(_)
        ));
    }

    /// An unknown option may well select an output shape, so it is refused rather than dropped.
    #[test]
    fn an_unknown_worker_option_is_refused() {
        for option in ["--brand-new", "--x_strict_java_nullability_assertions"] {
            assert!(matches!(
                translate(&args(&["--srcs", "A.kt", "--out", "o.jar", option])).unwrap_err(),
                Refusal::Unsupported(_)
            ));
        }
    }

    #[test]
    fn flagfiles_expand_one_argument_per_line() {
        let dir = std::env::temp_dir().join(format!("krusty-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("args.txt");
        std::fs::write(&path, "--out\no.jar\n--srcs\nA.kt\n").unwrap();
        let expanded =
            expand_flagfiles(&[format!("--flagfile={}", path.display())]).expect("expand");
        assert_eq!(expanded, args(&["--out", "o.jar", "--srcs", "A.kt"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The protocol loop: one JSON object per line in, one per line out, request ids preserved.
    #[test]
    fn the_protocol_round_trips_requests() {
        let requests = concat!(
            r#"{"arguments":["--srcs","A.kt","--out","o.jar"],"requestId":7}"#,
            "\n",
            r#"{"arguments":["--srcs","B.kts","--out","o.jar"],"requestId":8}"#,
            "\n",
        );
        let mut output = Vec::new();
        run(
            &mut std::io::BufReader::new(requests.as_bytes()),
            &mut output,
            &WorkerSetup::default(),
            &|_unit| Ok(()),
        )
        .expect("worker loop");
        let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
        assert_eq!(lines.len(), 2, "one response per request: {lines:?}");
        assert!(lines[0].contains("\"exitCode\":0") && lines[0].contains("\"requestId\":7"));
        // The refusal is a FAILED response, not a crash: bazel reports the action, the worker lives.
        assert!(lines[1].contains("\"exitCode\":1") && lines[1].contains("\"requestId\":8"));
        assert!(lines[1].contains("only .kt and .java sources"));
    }

    /// A compile failure is reported through the response, and the worker keeps serving.
    #[test]
    fn a_failed_compile_does_not_end_the_worker() {
        let requests = concat!(
            r#"{"arguments":["--srcs","A.kt","--out","o.jar"],"requestId":1}"#,
            "\n",
            r#"{"arguments":["--srcs","A.kt","--out","o.jar"],"requestId":2}"#,
            "\n",
        );
        let mut output = Vec::new();
        run(
            &mut std::io::BufReader::new(requests.as_bytes()),
            &mut output,
            &WorkerSetup::default(),
            &|_unit| Err(WorkFailure::new(FailureKind::Errors, "two errors")),
        )
        .expect("worker loop");
        let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.contains("\"exitCode\":1")));
        assert!(lines[1].contains("two errors"));
    }

    #[test]
    fn the_startup_command_line_configures_every_request() {
        let setup = WorkerSetup::from_startup(&args(&[
            "--kotlinc-arg",
            "-no-stdlib",
            "--jps-builder",
            "java",
            "--jps-builder",
            "builder.jar",
            "--persistent_worker",
        ]))
        .unwrap();
        assert_eq!(
            setup,
            WorkerSetup {
                kotlinc_args: args(&["-no-stdlib"]),
                handoff: Some(Handoff::new(args(&["java", "builder.jar"])).unwrap()),
            }
        );
        assert_eq!(
            WorkerSetup::from_startup(&args(&["--persistent_worker"])),
            Ok(WorkerSetup::default())
        );
        assert_eq!(
            WorkerSetup::from_startup(&args(&["--jps-builder"])),
            Err("--jps-builder needs a value".to_string())
        );
        assert_eq!(
            WorkerSetup::from_startup(&args(&["--srcs"])),
            Err("unknown worker startup argument `--srcs`".to_string())
        );
    }

    /// The build's kotlinc arguments precede the target's own, so the target's override them.
    #[test]
    fn startup_kotlinc_arguments_precede_the_targets() {
        let request = WorkRequest {
            arguments: args(&[
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
                "--kotlinc-arg",
                "-progressive",
            ]),
            ..WorkRequest::default()
        };
        let setup = WorkerSetup {
            kotlinc_args: args(&["-no-stdlib"]),
            handoff: None,
        };
        let response = serve(&request, &setup, &|unit| {
            assert_eq!(unit.kotlinc_args, args(&["-no-stdlib", "-progressive"]));
            Ok(())
        });
        assert_eq!(response.exit_code, 0, "{}", response.output);
    }

    /// A target krusty fails, or whose command line it rejects, is compiled by the JPS builder,
    /// and the response records why.
    #[cfg(unix)]
    #[test]
    fn a_target_krusty_does_not_compile_is_handed_to_the_jps_builder() {
        let setup = WorkerSetup {
            kotlinc_args: Vec::new(),
            handoff: Some(
                Handoff::new(args(&[
                    "sh",
                    "-c",
                    r"cat >/dev/null; printf '\012\010\000\022\006built\n'",
                ]))
                .unwrap(),
            ),
        };
        let failing = WorkRequest {
            arguments: args(&[
                "--srcs",
                "A.kt",
                "--out",
                "o.jar",
                "--target_label",
                "//a:a",
            ]),
            request_id: 3,
            ..WorkRequest::default()
        };
        let response = serve(&failing, &setup, &|_unit| {
            Err(WorkFailure::new(
                FailureKind::Unimplemented,
                "A.kt:1:1: error: e\n",
            ))
        });
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output: "krusty: target '//a:a' is left to the JPS builder: krusty does not \
                         implement everything the target uses\nA.kt:1:1: error: e\nbuilt\n"
                    .to_string(),
                request_id: 3,
            }
        );
        let rejected = WorkRequest {
            arguments: args(&["--srcs", "B.kts", "--out", "o.jar"]),
            ..WorkRequest::default()
        };
        let response = serve(&rejected, &setup, &|_unit| Ok(()));
        assert_eq!(
            response,
            WorkResponse {
                exit_code: 0,
                output:
                    "krusty: target 'this target' is left to the JPS builder: krusty rejects the \
                         command line\nkrusty: unsupported by krusty: --srcs B.kts: the worker \
                         compiles only .kt and .java sources\nbuilt\n"
                        .to_string(),
                request_id: 0,
            }
        );
    }

    #[test]
    fn an_unreadable_request_is_answered_not_fatal() {
        let mut output = Vec::new();
        run(
            &mut std::io::BufReader::new(&b"not json\n"[..]),
            &mut output,
            &WorkerSetup::default(),
            &|_unit| Ok(()),
        )
        .expect("worker loop");
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"exitCode\":1"), "{text}");
        assert!(text.contains("unreadable work request"));
    }
}
