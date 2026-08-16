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
//! What krusty CANNOT do here is stated as a failed work response rather than a silently wrong jar:
//! it has no Java front end, so a target whose action carries `.java` sources or source jars is
//! refused, and it produces no reduced ABI jar, so `--abi-out` receives a copy of the full jar.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use krusty::jvm::ir_emit::JvmDefaultMode;

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
    pub sources: Vec<PathBuf>,
    pub classpath: Vec<PathBuf>,
    /// `--friends`: modules whose `internal` declarations this target may see.
    pub friends: Vec<PathBuf>,
    pub module_name: Option<String>,
    pub target_label: Option<String>,
    /// The kotlinc-style flags translated from the worker's own option names.
    pub kotlinc_args: Vec<String>,
    pub jvm_default: JvmDefaultMode,
    pub param_assertions: bool,
    /// Worker options that were understood but have no effect on krusty's output.
    pub inert: Vec<String>,
}

/// Why a work request cannot be served.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The action mixes Java into the compilation. krusty has no Java front end, so producing a jar
    /// would silently drop those classes.
    JavaSources(String),
    /// A flag that changes the output shape in a way krusty does not implement.
    Unsupported(String),
    /// The request is malformed (a flag missing its value, no `--out`).
    Malformed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::JavaSources(detail) => write!(
                formatter,
                "krusty has no Java front end, so this target cannot be built by it: {detail}"
            ),
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
        "--x_context_parameters" => Some("-Xcontext-parameters"),
        "--x_context_receivers" => Some("-Xcontext-receivers"),
        "--x_inline_classes" => Some("-Xinline-classes"),
        "--x_skip_prerelease_check" => Some("-Xskip-prerelease-check"),
        "--x_when_guards" => Some("-Xwhen-guards"),
        // Diagnostics and reporting: they change what the compiler PRINTS, not what it emits.
        "--x_render_internal_diagnostic_names"
        | "--x_report_all_warnings"
        | "--trace"
        | "--no-proc"
        | "--x_strict_java_nullability_assertions"
        | "--x_wasm_attach_js_exception"
        | "--x_wasm_generate_closed_world_multimodule"
        | "--x_wasm_kclass_fqn" => None,
        _ => return None,
    })
}

/// Translate one `jvm-inc-builder` argument list into a [`WorkUnit`].
pub fn translate(arguments: &[String]) -> Result<WorkUnit, Refusal> {
    let mut unit = WorkUnit {
        param_assertions: true,
        ..WorkUnit::default()
    };
    let mut java_count: Option<usize> = None;
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
                        // A `.java` source in the action is the one case that must not be compiled
                        // "as far as possible": the jar would be missing those classes and every
                        // Kotlin reference to them would have failed to resolve.
                        if path.extension().and_then(|e| e.to_str()) == Some("java") {
                            return Err(Refusal::JavaSources(format!("{value} is a Java source")));
                        }
                        unit.sources.push(path);
                    }
                }
                "--src-jars" => source_jars.extend(values),
                "--cp" => unit.classpath.extend(values.into_iter().map(PathBuf::from)),
                // `associates` — the modules whose `internal` declarations this target may see.
                // krusty has no `-Xfriend-paths`, and it hides classpath `internal` members by
                // design, so accepting this would fail later with "unresolved" on every internal
                // reference instead of naming the real cause here.
                "--friends" => {
                    if !values.is_empty() {
                        return Err(Refusal::Unsupported(format!(
                            "--friends ({}): krusty cannot grant `internal` visibility across \
                             modules",
                            values.join(", ")
                        )));
                    }
                }
                "--opt_in" => unit
                    .kotlinc_args
                    .extend(values.into_iter().map(|value| format!("-opt-in={value}"))),
                "--x_xlanguage" => unit
                    .kotlinc_args
                    .extend(values.into_iter().map(|v| format!("-XXLanguage:{v}"))),
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
                unit.kotlinc_args.push("-jvm-target".to_string());
                unit.kotlinc_args.push(value_of(index, flag)?);
                index += 2;
            }
            "--api_version" => {
                unit.kotlinc_args.push("-api-version".to_string());
                unit.kotlinc_args.push(value_of(index, flag)?);
                index += 2;
            }
            "--language_version" => {
                unit.kotlinc_args.push("-language-version".to_string());
                unit.kotlinc_args.push(value_of(index, flag)?);
                index += 2;
            }
            // The four options this worker exists to honor. Each selects an artifact SHAPE, so a
            // value krusty does not emit is refused rather than compiled as something else.
            "--jvm_default" => {
                let value = value_of(index, flag)?;
                let mode = JvmDefaultMode::parse(&value)
                    .filter(|mode| mode.is_modelled())
                    .ok_or_else(|| Refusal::Unsupported(format!("--jvm_default {value}")))?;
                unit.jvm_default = mode;
                index += 2;
            }
            "--x_lambdas" | "--x_sam_conversions" => {
                let value = value_of(index, flag)?;
                if value != "indy" {
                    return Err(Refusal::Unsupported(format!("{flag} {value}")));
                }
                index += 2;
            }
            "--x_no_param_assertions" => {
                unit.param_assertions = false;
                index += 1;
            }
            "--x_no_call_assertions" => {
                // krusty emits no call assertions of its own, so its output already matches.
                unit.inert.push(flag.to_string());
                index += 1;
            }
            // A kotlinc flag forwarded verbatim by the rule (`kotlinc_opts`). Without this the rule
            // would have to re-spell every option in the worker vocabulary, and anything it could
            // not spell would silently not reach the compiler.
            "--kotlinc-arg" => {
                unit.kotlinc_args.push(value_of(index, flag)?);
                index += 2;
            }
            "--x_explicit_api" => {
                let value = value_of(index, flag)?;
                if value != "disable" {
                    unit.kotlinc_args.push(format!("-Xexplicit-api={value}"));
                }
                index += 2;
            }
            "--warn" => {
                let value = value_of(index, flag)?;
                if value == "off" {
                    unit.kotlinc_args.push("-nowarn".to_string());
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
            // `--resources` is followed by colon-joined `strip_prefix:add_prefix:file…` groups.
            // krusty's jar writer emits class files and the `kotlin_module` index, nothing else, so a
            // target with resources would get a jar silently missing them.
            "--resources" => {
                return Err(Refusal::Unsupported(
                    "--resources: krusty does not package resources into the output jar"
                        .to_string(),
                ));
            }
            // `--reduced-classpath-mode true` (and the `--direct-dependencies` list that accompanies
            // it) narrow what the builder puts on the classpath. Ignoring the optimization is safe:
            // krusty still receives the full `--cp`.
            "--reduced-classpath-mode" | "--x_compiler_plugin_order" => {
                unit.inert.push(flag.to_string());
                index += 1;
                // These carry a value; consume it rather than meeting it as a stray argument.
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
    if java_count.is_some_and(|count| count > 0) {
        return Err(Refusal::JavaSources(format!(
            "--java-count {}",
            java_count.unwrap_or_default()
        )));
    }
    if unit.output_jar.as_os_str().is_empty() {
        return Err(Refusal::Malformed("no --out".to_string()));
    }
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

/// One request of bazel's JSON worker protocol. Only the fields this worker reads are modelled;
/// bazel is free to send more.
#[derive(Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRequest {
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub request_id: i32,
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkResponse {
    pub exit_code: i32,
    pub output: String,
    pub request_id: i32,
}

/// Serve one request. Returns the response bazel should receive.
pub fn serve(
    request: &WorkRequest,
    compile: &dyn Fn(WorkUnit) -> Result<(), String>,
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
    match translate(&arguments).map_err(|refusal| refusal.to_string()) {
        Ok(unit) => {
            // Report what was understood but had no effect. `WorkResponse::output` is the channel
            // bazel prints, and silently dropping an option the target set is the habit this module
            // exists to avoid.
            let note = if unit.inert.is_empty() {
                String::new()
            } else {
                format!("krusty: no effect on output: {}\n", unit.inert.join(" "))
            };
            match compile(unit) {
                Ok(()) => WorkResponse {
                    exit_code: 0,
                    output: note,
                    request_id: request.request_id,
                },
                Err(error) => WorkResponse {
                    exit_code: 1,
                    output: format!("{note}{error}"),
                    request_id: request.request_id,
                },
            }
        }
        Err(error) => WorkResponse {
            exit_code: 1,
            output: format!("krusty: {error}"),
            request_id: request.request_id,
        },
    }
}

/// Read work requests until stdin closes, one JSON object per line.
pub fn run(
    input: &mut dyn BufRead,
    output: &mut dyn Write,
    compile: &dyn Fn(WorkUnit) -> Result<(), String>,
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
            Ok(request) => serve(&request, compile),
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
        assert_eq!(unit.jvm_default, JvmDefaultMode::NoCompatibility);
        assert!(unit.param_assertions);
        for expected in [
            "-jvm-target",
            "25",
            "-api-version",
            "2.4",
            "-progressive",
            "-nowarn",
            "-XXLanguage:+AllowEagerSupertypeAccessibilityChecks",
        ] {
            assert!(
                unit.kotlinc_args.iter().any(|a| a == expected),
                "{expected} missing from {:?}",
                unit.kotlinc_args
            );
        }
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
        assert_eq!(unit.jvm_default, JvmDefaultMode::NoCompatibility);
    }

    /// A mode krusty does not emit fails the action instead of producing another shape.
    #[test]
    fn an_unemittable_jvm_default_is_refused() {
        let refusal = translate(&args(&[
            "--jvm_default",
            "disable",
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
        assert!(!unit.param_assertions);
        assert!(unit.inert.iter().any(|f| f == "--x_no_call_assertions"));
    }

    /// The class-based lambda strategy is a different class set, so it is refused like any other
    /// shape krusty cannot emit.
    #[test]
    fn a_non_indy_lambda_strategy_is_refused() {
        for flag in ["--x_lambdas", "--x_sam_conversions"] {
            let refusal =
                translate(&args(&[flag, "class", "--srcs", "A.kt", "--out", "o.jar"])).unwrap_err();
            assert!(
                matches!(refusal, Refusal::Unsupported(_)),
                "{flag}: {refusal:?}"
            );
        }
    }

    /// The honest limit: krusty has no Java front end, so a mixed target is refused rather than
    /// compiled into a jar missing every Java class.
    #[test]
    fn java_in_the_action_is_refused_two_ways() {
        let by_source =
            translate(&args(&["--srcs", "A.kt", "B.java", "--out", "o.jar"])).unwrap_err();
        assert!(
            matches!(by_source, Refusal::JavaSources(_)),
            "{by_source:?}"
        );

        let by_count = translate(&args(&[
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
            "--java-count",
            "3",
        ]))
        .unwrap_err();
        assert!(matches!(by_count, Refusal::JavaSources(_)), "{by_count:?}");
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
    }

    /// Resources would be dropped from the jar, so the target is refused rather than shipped
    /// incomplete.
    #[test]
    fn a_target_with_resources_is_refused() {
        let refusal = translate(&args(&[
            "--resources",
            "res:prefix:a.txt",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
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
            "--kotlinc-arg",
            "-progressive",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .expect("must translate");
        assert_eq!(
            unit.kotlinc_args,
            vec!["-Xjvm-default=all".to_string(), "-progressive".to_string()]
        );
    }

    /// `--friends` carries the modules whose `internal` declarations must be visible. krusty has no
    /// equivalent and hides classpath `internal` members, so accepting it would fail later with
    /// "unresolved" on every internal reference instead of naming the cause here.
    #[test]
    fn associates_are_refused_rather_than_dropped() {
        let refusal = translate(&args(&[
            "--friends",
            "peer.jar",
            "--srcs",
            "A.kt",
            "--out",
            "o.jar",
        ]))
        .unwrap_err();
        assert!(matches!(refusal, Refusal::Unsupported(_)), "{refusal:?}");
    }

    /// Options understood but without effect are REPORTED, through the channel bazel prints.
    #[test]
    fn inert_options_are_reported_in_the_response() {
        let request = WorkRequest {
            arguments: args(&["--x_no_call_assertions", "--srcs", "A.kt", "--out", "o.jar"]),
            request_id: 4,
            cancel: false,
        };
        let response = serve(&request, &|_unit| Ok(()));
        assert_eq!(response.exit_code, 0);
        assert!(
            response.output.contains("--x_no_call_assertions"),
            "an inert option must be surfaced: {:?}",
            response.output
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
        assert!(matches!(
            translate(&args(&["--srcs", "A.kt", "--out", "o.jar", "--brand-new"])).unwrap_err(),
            Refusal::Unsupported(_)
        ));
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
            r#"{"arguments":["--srcs","B.java","--out","o.jar"],"requestId":8}"#,
            "\n",
        );
        let mut output = Vec::new();
        run(
            &mut std::io::BufReader::new(requests.as_bytes()),
            &mut output,
            &|_unit| Ok(()),
        )
        .expect("worker loop");
        let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
        assert_eq!(lines.len(), 2, "one response per request: {lines:?}");
        assert!(lines[0].contains("\"exitCode\":0") && lines[0].contains("\"requestId\":7"));
        // The refusal is a FAILED response, not a crash: bazel reports the action, the worker lives.
        assert!(lines[1].contains("\"exitCode\":1") && lines[1].contains("\"requestId\":8"));
        assert!(lines[1].contains("Java front end"));
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
            &|_unit| Err("two errors".to_string()),
        )
        .expect("worker loop");
        let lines: Vec<&str> = std::str::from_utf8(&output).unwrap().lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.contains("\"exitCode\":1")));
        assert!(lines[1].contains("two errors"));
    }

    #[test]
    fn an_unreadable_request_is_answered_not_fatal() {
        let mut output = Vec::new();
        run(
            &mut std::io::BufReader::new(&b"not json\n"[..]),
            &mut output,
            &|_unit| Ok(()),
        )
        .expect("worker loop");
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"exitCode\":1"), "{text}");
        assert!(text.contains("unreadable work request"));
    }
}
