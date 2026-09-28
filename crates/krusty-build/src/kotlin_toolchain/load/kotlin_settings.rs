//! `settings.kotlin` as the compiler arguments a JVM module is built with.
//!
//! Typed options are emitted in a fixed order, then `freeCompilerArgs`, so a repeated flag in the
//! free list is the one the compiler sees last. `explicitApi` is omitted from test compilations.
//! Selecting a Kotlin distribution, running a compiler plugin, and native-only options stay refused.

use std::path::Path;

use super::super::yaml::Yaml;
use crate::compiler::unsupported_opaque_argument;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct KotlinSettings {
    pub main_args: Vec<String>,
    pub test_args: Vec<String>,
}

pub(super) fn parse(file: &Path, value: &Yaml) -> Result<KotlinSettings, String> {
    let Some(entries) = value.as_map() else {
        return Err(format!("{}: settings must be a mapping", file.display()));
    };
    let mut settings = KotlinSettings::default();
    for (key, nested) in entries {
        match key.as_str() {
            "kotlin" => settings = parse_kotlin(file, nested)?,
            other => {
                return Err(format!(
                    "{}: unsupported settings key '{other}'",
                    file.display()
                ));
            }
        }
    }
    Ok(settings)
}

struct Options {
    api_version: Option<String>,
    language_version: Option<String>,
    opt_ins: Vec<String>,
    all_warnings_as_errors: bool,
    progressive: bool,
    suppress_warnings: bool,
    verbose: bool,
    explicit_api: Option<String>,
    free_compiler_args: Vec<String>,
}

fn parse_kotlin(file: &Path, value: &Yaml) -> Result<KotlinSettings, String> {
    let Some(entries) = value.as_map() else {
        return Err(format!(
            "{}: settings.kotlin must be a mapping",
            file.display()
        ));
    };
    let mut options = Options {
        api_version: None,
        language_version: None,
        opt_ins: Vec::new(),
        all_warnings_as_errors: false,
        progressive: false,
        suppress_warnings: false,
        verbose: false,
        explicit_api: None,
        free_compiler_args: Vec::new(),
    };
    for (key, nested) in entries {
        match key.as_str() {
            "apiVersion" => options.api_version = Some(version(file, "apiVersion", nested)?),
            "languageVersion" => {
                options.language_version = Some(version(file, "languageVersion", nested)?);
            }
            "optIns" => options.opt_ins = string_list(file, "optIns", nested)?,
            "allWarningsAsErrors" => {
                options.all_warnings_as_errors = boolean(file, "allWarningsAsErrors", nested)?;
            }
            "progressiveMode" => {
                options.progressive = boolean(file, "progressiveMode", nested)?;
            }
            "suppressWarnings" => {
                options.suppress_warnings = boolean(file, "suppressWarnings", nested)?;
            }
            "verbose" => options.verbose = boolean(file, "verbose", nested)?,
            "explicitApi" => options.explicit_api = explicit_api(file, nested)?,
            "freeCompilerArgs" => {
                options.free_compiler_args = string_list(file, "freeCompilerArgs", nested)?;
            }
            "compileIncrementally" => {
                if boolean(file, "compileIncrementally", nested)? {
                    return Err(format!(
                        "{}: settings.kotlin.compileIncrementally cannot be enabled; \
                         krusty-toolchain build compiles each module as a whole",
                        file.display()
                    ));
                }
            }
            "version" => {
                return Err(format!(
                    "{}: unsupported settings.kotlin key 'version'; \
                     krusty-toolchain build does not select a Kotlin compiler version yet",
                    file.display()
                ));
            }
            "allOpen" | "compilerPlugins" | "dataframe" | "jsPlainObjects" | "ksp" | "noArg"
            | "powerAssert" | "rpc" | "serialization" => {
                return Err(format!(
                    "{}: unsupported settings.kotlin key '{key}'; \
                     krusty-toolchain build does not run compiler plugins yet",
                    file.display()
                ));
            }
            "debug" | "linkerOptions" | "optimization" => {
                return Err(format!(
                    "{}: settings.kotlin key '{key}' applies to native targets; \
                     krusty-toolchain build compiles the JVM",
                    file.display()
                ));
            }
            other => {
                return Err(format!(
                    "{}: unsupported settings.kotlin key '{other}'",
                    file.display()
                ));
            }
        }
    }
    if let Some(argument) = unsupported_opaque_argument(&options.free_compiler_args) {
        return Err(free_arg_error(file, argument));
    }
    Ok(emit(options))
}

fn emit(options: Options) -> KotlinSettings {
    let mut args = Vec::new();
    push_value(&mut args, "-api-version", options.api_version.as_deref());
    push_value(
        &mut args,
        "-language-version",
        options.language_version.as_deref(),
    );
    for opt_in in &options.opt_ins {
        args.push("-opt-in".to_string());
        args.push(opt_in.clone());
    }
    if options.all_warnings_as_errors {
        args.push("-Werror".to_string());
    }
    if options.progressive {
        args.push("-progressive".to_string());
    }
    if options.suppress_warnings {
        args.push("-nowarn".to_string());
    }
    if options.verbose {
        args.push("-verbose".to_string());
    }
    let mut test_args = args.clone();
    if let Some(mode) = &options.explicit_api {
        args.push("-Xexplicit-api".to_string());
        args.push(mode.clone());
    }
    args.extend(options.free_compiler_args.iter().cloned());
    test_args.extend(options.free_compiler_args);
    KotlinSettings {
        main_args: args,
        test_args,
    }
}

fn push_value(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        args.push(flag.to_string());
        args.push(value.to_string());
    }
}

fn version(file: &Path, field: &str, value: &Yaml) -> Result<String, String> {
    let text = super::scalar(file, field, value)?;
    if text.is_empty() {
        return Err(format!("{}: {field} must not be empty", file.display()));
    }
    Ok(text)
}

fn boolean(file: &Path, field: &str, value: &Yaml) -> Result<bool, String> {
    match super::scalar(file, field, value)?.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "{}: {field} must be true or false, found '{other}'",
            file.display()
        )),
    }
}

fn explicit_api(file: &Path, value: &Yaml) -> Result<Option<String>, String> {
    let mode = super::scalar(file, "explicitApi", value)?;
    match mode.as_str() {
        "disable" => Ok(None),
        "strict" | "warning" => Ok(Some(mode)),
        other => Err(format!(
            "{}: explicitApi must be strict, warning, or disable, found '{other}'",
            file.display()
        )),
    }
}

fn string_list(file: &Path, field: &str, value: &Yaml) -> Result<Vec<String>, String> {
    let Some(items) = value.as_seq() else {
        if matches!(value, Yaml::Scalar(text) if text.is_empty()) {
            return Ok(Vec::new());
        }
        return Err(format!("{}: {field} must be a list", file.display()));
    };
    let mut values = Vec::new();
    for item in items {
        let text = super::scalar(file, field, item)?;
        if text.is_empty() {
            return Err(format!(
                "{}: {field} entries must not be empty",
                file.display()
            ));
        }
        values.push(text);
    }
    Ok(values)
}

fn free_arg_error(file: &Path, argument: &str) -> String {
    if matches!(
        argument,
        "-language-version" | "-api-version" | "-opt-in" | "-Xexplicit-api"
    ) {
        format!(
            "{}: settings.kotlin.freeCompilerArgs flag '{argument}' needs its value in the same list",
            file.display()
        )
    } else {
        format!(
            "{}: settings.kotlin.freeCompilerArgs entry '{argument}' can introduce an unmodelled \
             input; record that input in the module model instead",
            file.display()
        )
    }
}
