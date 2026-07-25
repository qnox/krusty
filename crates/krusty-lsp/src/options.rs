//! Process options specific to the language-server executable.

use std::path::{Path, PathBuf};

use krusty::features::LangFeatures;
use krusty::jvm::classpath::platform_jdk_modules;

#[derive(Default)]
pub struct LspOptions {
    classpath: Vec<PathBuf>,
    jdk_home: Option<PathBuf>,
    no_jdk: bool,
    language_features: LangFeatures,
    language_arguments: Vec<String>,
}

impl LspOptions {
    pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = argv.into_iter();
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--stdio" => {}
                "-cp" | "-classpath" | "-class-path" => {
                    let value = args
                        .next()
                        .ok_or_else(|| format!("{argument} requires a value"))?;
                    options.classpath.extend(std::env::split_paths(&value));
                }
                "-jdk-home" => {
                    options.jdk_home = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| "-jdk-home requires a value".to_string())?,
                    ));
                }
                "-no-jdk" => options.no_jdk = true,
                _ => {
                    if options.language_features.apply_cli_arg(&argument) {
                        options.language_arguments.push(argument);
                    } else {
                        return Err(format!("unsupported option '{argument}'"));
                    }
                }
            }
        }
        Ok(options)
    }

    pub fn effective_classpath(&self) -> Vec<PathBuf> {
        let mut classpath = self.classpath.clone();
        if !self.no_jdk {
            if let Some(modules) = platform_jdk_modules(self.jdk_home.as_deref()) {
                classpath.push(modules);
            }
        }
        classpath
    }

    /// The classpath passed explicitly with `-cp`, without JDK modules. Empty means "no explicit
    /// classpath" — the trigger for the server to resolve one from the build tool.
    pub fn explicit_classpath(&self) -> &[PathBuf] {
        &self.classpath
    }

    /// The `-jdk-home` hint, if one was given.
    pub fn jdk_home(&self) -> Option<&Path> {
        self.jdk_home.as_deref()
    }

    /// Whether `-no-jdk` was passed: the server must not attach a JDK.
    pub fn no_jdk(&self) -> bool {
        self.no_jdk
    }

    pub fn language_features(&self) -> &LangFeatures {
        &self.language_features
    }

    /// Apply explicit LSP flags over project-derived features, preserving CLI order and disables.
    pub fn apply_language_features(&self, features: &mut LangFeatures) {
        for argument in &self.language_arguments {
            features.apply_cli_arg(argument);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LspOptions, String> {
        LspOptions::parse(args.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn accepts_only_language_server_process_options() {
        let options = parse(&[
            "--stdio",
            "-cp",
            "a.jar:b/classes",
            "-no-jdk",
            "-Xname-based-destructuring=complete",
        ])
        .unwrap();
        assert_eq!(
            options.effective_classpath(),
            vec![PathBuf::from("a.jar"), PathBuf::from("b/classes")]
        );
        assert!(options.language_features().has("NameBasedDestructuring"));
        let options = parse(&[
            "-Xname-based-destructuring=complete",
            "-Xname-based-destructuring=disable",
        ])
        .unwrap();
        assert!(!options.language_features().has("NameBasedDestructuring"));
        assert!(parse(&["Main.kt"]).is_err());
        assert!(parse(&["-d", "out"]).is_err());
    }

    #[test]
    fn missing_option_values_are_errors() {
        assert_eq!(
            parse(&["-cp"]).err().as_deref(),
            Some("-cp requires a value")
        );
        assert_eq!(
            parse(&["-jdk-home"]).err().as_deref(),
            Some("-jdk-home requires a value")
        );
    }
}
