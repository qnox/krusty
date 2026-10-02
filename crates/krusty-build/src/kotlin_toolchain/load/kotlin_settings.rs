//! `settings.kotlin` as the compiler arguments a JVM module is built with.
//!
//! Only options whose semantic effect `krusty` owns are forwarded. Settings that the compiler CLI
//! currently accepts but ignores fail here rather than producing a successful build with the wrong
//! language, API, diagnostic, or explicit-API policy.

use std::path::Path;

use super::super::yaml::Yaml;

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

fn parse_kotlin(file: &Path, value: &Yaml) -> Result<KotlinSettings, String> {
    let Some(entries) = value.as_map() else {
        return Err(format!(
            "{}: settings.kotlin must be a mapping",
            file.display()
        ));
    };
    let mut free_compiler_args = Vec::new();
    for (key, nested) in entries {
        match key.as_str() {
            "freeCompilerArgs" => {
                free_compiler_args = string_list(file, "freeCompilerArgs", nested)?;
            }
            "apiVersion"
            | "languageVersion"
            | "optIns"
            | "allWarningsAsErrors"
            | "progressiveMode"
            | "suppressWarnings"
            | "verbose"
            | "explicitApi"
            | "compileIncrementally" => return Err(unsupported_semantic_setting(file, key)),
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
    if let Some(argument) = free_compiler_args
        .iter()
        .find(|argument| !supported_free_argument(argument))
    {
        return Err(format!(
            "{}: unsupported settings.kotlin.freeCompilerArgs entry '{argument}'; \
             krusty-toolchain build forwards only compiler options whose semantic effect krusty owns",
            file.display()
        ));
    }
    Ok(KotlinSettings {
        main_args: free_compiler_args.clone(),
        test_args: free_compiler_args,
    })
}

fn unsupported_semantic_setting(file: &Path, key: &str) -> String {
    format!(
        "{}: unsupported settings.kotlin key '{key}'; \
         krusty does not yet enforce this compiler setting",
        file.display()
    )
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

fn supported_free_argument(argument: &str) -> bool {
    matches!(argument, "-Xcontext-parameters" | "-Xno-param-assertions")
}
