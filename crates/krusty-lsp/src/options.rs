//! Process options specific to the language-server executable.

use std::path::PathBuf;

use krusty::jvm::classpath::platform_jdk_modules;

#[derive(Default)]
pub struct LspOptions {
    classpath: Vec<PathBuf>,
    jdk_home: Option<PathBuf>,
    no_jdk: bool,
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
                _ => return Err(format!("unsupported option '{argument}'")),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<LspOptions, String> {
        LspOptions::parse(args.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn accepts_only_language_server_process_options() {
        let options = parse(&["--stdio", "-cp", "a.jar:b/classes", "-no-jdk"]).unwrap();
        assert_eq!(
            options.effective_classpath(),
            vec![PathBuf::from("a.jar"), PathBuf::from("b/classes")]
        );
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
