//! Applying parsed kotlinc arguments to [`Options`]: the arguments krusty implements, the language
//! settings they select, and the source inputs.

use std::path::PathBuf;

use krusty::jvm::ir_emit::{JvmDefaultMode, LambdaMode};
use krusty::kotlin_version::KotlinVersion;
use krusty::language_settings::LanguageSettings;
use krusty::language_version::LanguageVersion;

use super::{jvm_target_to_major, supported_kotlin_versions, CliWarning, Options, WarningName};
use crate::kotlinc_arguments::{Occurrence, Value};

/// Record a `-jvm-default`/`-Xjvm-default` value, reporting one krusty does not model instead of
/// silently compiling under a different interface shape than the build asked for.
fn apply_jvm_default(
    opts: &mut Options,
    flag: &str,
    value: &str,
    parse_value: fn(&str) -> Option<JvmDefaultMode>,
) {
    // An unknown value is fatal. Merely recording it as an ignored option would let the driver
    // continue with `Enable`, silently emitting a different interface shape than the invocation
    // requested. Every value kotlinc accepts is emitted, so there is no second rejection class.
    match parse_value(value) {
        Some(mode) => opts.jvm_default = mode,
        None => opts
            .errors
            .push(format!("invalid value '{value}' for {flag}")),
    }
}

fn supported_stamp_levels() -> String {
    LanguageVersion::supported_metadata_stamps_text()
}

/// Parse a `major.minor` level on the shared stamp contract. Anything else — a patch segment,
/// a sign, or a level outside the internal 2.0 through 2.4 stamp domain — is unknown.
fn parse_metadata_level(value: &str) -> Option<[i32; 3]> {
    LanguageVersion::parse_supported_metadata_stamp(value).map(LanguageVersion::metadata_version)
}

/// The named configuration warnings in kotlinc's order: language/API version status, argument
/// lifecycle, then redundant feature arguments.
fn settings_warnings(
    settings: &LanguageSettings,
    feature_arguments: &[String],
    suppress_version_warnings: bool,
    lifecycle: Vec<CliWarning>,
) -> Vec<CliWarning> {
    let mut warnings = Vec::new();
    if suppress_version_warnings {
        warnings.extend(lifecycle);
        warnings.extend(redundant_feature_warnings(settings, feature_arguments));
        return warnings;
    }
    if settings.language_version >= LanguageVersion::V2_2
        && settings.api_version <= LanguageVersion::V2_1
    {
        warnings.push(CliWarning {
            name: WarningName::DeprecatedLanguageVersion,
            message: format!(
                "API version {} is deprecated and its support will be removed in a future version of Kotlin. Update the version to 2.2.",
                settings.api_version
            ),
        });
    }
    match settings.language_version {
        LanguageVersion::V2_0 | LanguageVersion::V2_1 => warnings.push(CliWarning {
            name: WarningName::DeprecatedLanguageVersion,
            message: format!(
                "language version {} is deprecated and its support will be removed in a future version of Kotlin. Update the version to 2.2.",
                settings.language_version
            ),
        }),
        LanguageVersion::V2_5 | LanguageVersion::V2_6 => warnings.push(CliWarning {
            name: WarningName::ExperimentalLanguageVersion,
            message: format!(
                "language version {} is experimental, there are no backwards compatibility guarantees for new language and library features. Use the stable version 2.4 instead.",
                settings.language_version
            ),
        }),
        _ => {}
    }
    warnings.extend(lifecycle);
    warnings.extend(redundant_feature_warnings(settings, feature_arguments));
    warnings
}

fn redundant_feature_warnings(
    settings: &LanguageSettings,
    feature_arguments: &[String],
) -> Vec<CliWarning> {
    let mut warnings = Vec::new();
    let mut applied = krusty::features::LangFeatures::for_versions(
        settings.language_version,
        settings.api_version,
    );
    for argument in feature_arguments {
        let before = applied.clone();
        let recognized = applied.apply_cli_arg(argument);
        debug_assert!(
            recognized,
            "the parser retained only language feature arguments"
        );
        if applied == before {
            warnings.push(CliWarning {
                name: WarningName::RedundantCliArg,
                message: format!(
                    "the argument '{argument}' is redundant for the current language version {}.",
                    settings.language_version
                ),
            });
        }
    }
    warnings
}

/// Split a classpath string on the platform separator (`:` on Unix).
fn split_classpath(v: &str) -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    v.split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub(super) fn collect_sources(path: &str, out: &mut Vec<String>, ignored: &mut Vec<String>) {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for e in entries {
                collect_sources(&e.to_string_lossy(), out, ignored);
            }
        }
    } else if krusty::source::is_batch_compilable_path(p) {
        out.push(path.to_string());
    } else if krusty::source::kind(p).is_some() {
        ignored.push(format!("{path} (script compilation is not supported yet)"));
    }
}

/// `-Xkotlin-reference-version` is krusty's own argument: it selects the kotlinc release whose
/// argument table parses everything else, so it is read before that table is chosen.
pub(super) fn take_reference_version(arguments: Vec<String>, opts: &mut Options) -> Vec<String> {
    let mut rest = Vec::with_capacity(arguments.len());
    let mut sources_only = false;
    for argument in arguments {
        sources_only |= argument == "--";
        let value = (!sources_only)
            .then(|| argument.strip_prefix("-Xkotlin-reference-version="))
            .flatten();
        let Some(value) = value else {
            rest.push(argument);
            continue;
        };
        match KotlinVersion::parse(value)
            .filter(|version| KotlinVersion::supported().contains(version))
        {
            Some(version) => opts.kotlin_reference_version = Some(version),
            None => opts.errors.push(format!(
                "-Xkotlin-reference-version={value} names no supported Kotlin release \
                 (supported: {})",
                supported_kotlin_versions()
            )),
        }
    }
    rest
}

/// The language and API settings read from the command line, applied once every argument is known
/// so explicit `-XXLanguage` overrides stay ordered independently of where the versions appear.
#[derive(Default)]
pub(super) struct ParsedSettings {
    language_version: Option<String>,
    api_version: Option<String>,
    language_feature_arguments: Vec<String>,
}

/// The argument as kotlinc would spell it, for the note that krusty dropped it.
pub(super) fn rendered(occurrence: &Occurrence) -> String {
    let name = &occurrence.spec.name;
    match &occurrence.value {
        Value::Bool(true) => name.clone(),
        Value::Bool(false) => format!("{name}=false"),
        Value::String(value) => format!("{name}={value}"),
        Value::List(values) => format!("{name}={}", values.join(",")),
    }
}

pub(super) fn apply(opts: &mut Options, parsed: &mut ParsedSettings, occurrence: &Occurrence) {
    let spec = occurrence.spec;
    let name = spec.name.as_str();
    let (flag, text, list) = match &occurrence.value {
        Value::Bool(flag) => (*flag, "", &[][..]),
        Value::String(text) => (true, text.as_str(), &[][..]),
        Value::List(values) => (true, "", values.as_slice()),
    };
    // `@Enables`/`@Disables` arguments are language-feature switches; `-XXLanguage` is the raw form.
    if spec.changes_language_features() || name == "-XXLanguage" {
        let spellings: Vec<String> = match &occurrence.value {
            Value::Bool(_) => vec![name.to_string()],
            Value::String(value) => vec![format!("{name}={value}")],
            Value::List(values) => values
                .iter()
                .map(|value| format!("{name}:{value}"))
                .collect(),
        };
        for spelling in spellings {
            if opts.language_settings.features.apply_cli_arg(&spelling) {
                parsed.language_feature_arguments.push(spelling);
            } else {
                opts.ignored.push(spelling);
            }
        }
        return;
    }
    match name {
        "-d" => opts.dest = PathBuf::from(text),
        // A string argument: the last occurrence wins, as in kotlinc.
        "-classpath" => opts.classpath = split_classpath(text),
        "-Xfriend-paths" => opts.friend_paths.extend(
            list.iter()
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
        ),
        "-module-name" => opts.module_name = text.to_string(),
        "-language-version" => parsed.language_version = Some(text.to_string()),
        "-api-version" => parsed.api_version = Some(text.to_string()),
        "-Xmetadata-version" => match parse_metadata_level(text) {
            Some(version) => opts.metadata_version = Some(version),
            None => opts.errors.push(format!(
                "unknown metadata version: {text}\nSupported metadata versions: {}",
                supported_stamp_levels()
            )),
        },
        // `-Xwarning-level=<NAME>:<SEVERITY>` configures one exact diagnostic identity. Like
        // kotlinc, reject an unknown or repeated name; retaining an opaque spelling as an ignored
        // option would claim a warning policy the compiler never applies.
        "-Xwarning-level" => {
            for value in list {
                if let Err(error) = opts.warning_policy.configure(value) {
                    opts.errors.push(error);
                }
            }
        }
        "-jdk-home" => opts.jdk_home = Some(PathBuf::from(text)),
        "-Xno-param-assertions" => opts.no_param_assertions = flag,
        "-Xno-call-assertions" => opts.no_call_assertions = flag,
        // `-Xlambdas` / `-Xsam-conversions` select how a lambda and a SAM conversion are realized:
        // `indy` is a `LambdaMetafactory` call site (kotlinc's default since 2.0), `class` gives each
        // lambda its own class extending `kotlin.jvm.internal.Lambda`. The two differ in the emitted
        // class SET, so each value selects its own emitter strategy rather than being advisory.
        "-Xlambdas" | "-Xsam-conversions" => {
            let mode = match text {
                "indy" => LambdaMode::Indy,
                "class" => LambdaMode::Class,
                _ => {
                    opts.errors.push(format!(
                        "{name}={text} selects an output shape krusty does not emit"
                    ));
                    return;
                }
            };
            if name == "-Xlambdas" {
                opts.lambda_modes.lambdas = mode;
            } else {
                opts.lambda_modes.sam_conversions = mode;
            }
        }
        "-no-stdlib" => opts.no_stdlib = flag,
        "-no-reflect" => opts.no_reflect = flag,
        "-no-jdk" => opts.no_jdk = flag,
        // Honor the target: it sets the emitted class-file version. An unrecognized value is
        // reported like any other ignored option rather than silently defaulting.
        "-jvm-target" => match jvm_target_to_major(text) {
            Some(major) => opts.jvm_target_major = Some(major),
            None => opts.ignored.push(format!("-jvm-target {text}")),
        },
        // `-jvm-default` decides the JVM shape of an interface's members with bodies; the legacy
        // `-Xjvm-default` spelling names the same three shapes differently.
        "-jvm-default" => apply_jvm_default(opts, name, text, JvmDefaultMode::parse),
        "-Xjvm-default" => apply_jvm_default(opts, name, text, JvmDefaultMode::parse_legacy),
        // kotlinc's `-X` help: suppress warnings about outdated, inconsistent, or experimental
        // language or API versions. A warning that is not queued cannot be promoted by
        // `-Xwarning-level` either.
        "-Xsuppress-version-warnings" => opts.suppress_version_warnings = flag,
        // `-Xexplicit-api=<mode>` requires public API to state its visibility and types. It
        // selects diagnostics, not a language feature, so `disable` is never redundant.
        "-Xexplicit-api" => {
            if matches!(text, "strict" | "warning" | "disable") {
                opts.explicit_api = Some(text.to_string());
            } else {
                opts.errors.push(format!(
                    "unknown value for parameter -Xexplicit-api: '{text}'. Value should be \
                     one of {{disable, strict, warning}}"
                ));
            }
        }
        "-opt-in" => opts
            .opt_in
            .extend(list.iter().filter(|marker| !marker.is_empty()).cloned()),
        // Compiler plugins, in kotlinc's syntax. They are resolved against the extension registry
        // before compiling, never dropped: a plugin that changes the output must either run
        // natively or fail the compile.
        "-Xplugin" | "-Xcompiler-plugin" => {
            for value in list {
                opts.plugins.accept(&format!("{name}={value}"), || None);
            }
        }
        "-P" => {
            for value in list {
                opts.plugins.accept(&format!("-P={value}"), || None);
            }
        }
        "-java-parameters" => opts.java_parameters = flag,
        "-version" => opts.print_version = flag,
        "-help" | "-X" => opts.print_help |= flag,
        _ => unreachable!("{name} is listed as applied but has no rule"),
    }
}

pub(super) fn finish_language_settings(
    opts: &mut Options,
    parsed: ParsedSettings,
    reference_version: KotlinVersion,
    lifecycle: Vec<CliWarning>,
) {
    let language_version = match parsed.language_version {
        Some(text) => match LanguageVersion::parse_supported_for(&text, reference_version) {
            Some(version) => version,
            None => {
                opts.errors.push(format!(
                    "unknown language version: {text}\nSupported language versions: {}",
                    LanguageVersion::supported_text_for(reference_version)
                ));
                LanguageVersion::default_for(reference_version)
            }
        },
        None => LanguageVersion::default_for(reference_version),
    };
    let api_version = match parsed.api_version {
        Some(text) => match LanguageVersion::parse_supported_for(&text, reference_version) {
            Some(version) => Some(version),
            None => {
                opts.errors.push(format!(
                    "unknown API version: {text}\nSupported API versions: {}",
                    LanguageVersion::supported_text_for(reference_version)
                ));
                None
            }
        },
        None => None,
    };
    let feature_arguments = &parsed.language_feature_arguments;
    match LanguageSettings::new(language_version, api_version, feature_arguments) {
        Ok(settings) => {
            opts.warnings = settings_warnings(
                &settings,
                feature_arguments,
                opts.suppress_version_warnings,
                lifecycle,
            );
            opts.language_settings = settings;
            if let Some(mode) = &opts.explicit_api {
                opts.language_settings
                    .features
                    .apply_explicit_api_mode(mode);
            }
            for marker in &opts.opt_in {
                opts.language_settings.features.opt_in(marker);
            }
        }
        Err(error) => opts.errors.push(error),
    }
}
