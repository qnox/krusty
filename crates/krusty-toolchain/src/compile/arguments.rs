//! The compiler arguments of one module's main fragment, in the order the toolchain passes them to
//! the Kotlin compiler (through the Build Tools API, which krusty receives as a command line).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What one compilation is made of.
pub(super) struct Compilation<'a> {
    /// `jvm.release`, when set.
    pub release: Option<i32>,
    pub jdk_home: &'a Path,
    pub classpath: &'a [PathBuf],
    pub module_name: &'a str,
    /// `jvm.storeParameterNames`.
    pub java_parameters: bool,
    /// Compiler plugin jars, in order.
    pub plugins: &'a [PathBuf],
    /// `kotlin.version`: the Kotlin release krusty compiles as.
    pub kotlin_version: &'a str,
    pub output: &'a Path,
    /// The source files, sorted.
    pub sources: &'a [PathBuf],
}

fn joined(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

/// The arguments of `compilation`, the program name excluded.
pub(super) fn arguments(compilation: &Compilation<'_>) -> Result<Vec<OsString>, String> {
    let mut arguments = Vec::new();
    if let Some(release) = compilation.release {
        arguments.push(OsString::from(format!("-Xjdk-release={release}")));
    }
    arguments.push(joined("-jdk-home=", compilation.jdk_home));
    let classpath = std::env::join_paths(compilation.classpath).map_err(|error| {
        format!(
            "module `{}` has a classpath entry krusty cannot be given: {error}",
            compilation.module_name
        )
    })?;
    let mut option = OsString::from("-classpath=");
    option.push(classpath);
    arguments.push(option);
    arguments.push(OsString::from("-no-stdlib"));
    arguments.push(OsString::from(format!(
        "-module-name={}",
        compilation.module_name
    )));
    if compilation.java_parameters {
        arguments.push(OsString::from("-java-parameters"));
    }
    for plugin in compilation.plugins {
        arguments.push(joined("-Xplugin=", plugin));
    }
    arguments.push(OsString::from(format!(
        "-Xkotlin-reference-version={}",
        compilation.kotlin_version
    )));
    arguments.push(OsString::from("-d"));
    arguments.push(compilation.output.as_os_str().to_owned());
    arguments.extend(
        compilation
            .sources
            .iter()
            .map(|source| source.as_os_str().to_owned()),
    );
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_follow_the_toolchain_order() {
        let classpath = [PathBuf::from("/out/lib"), PathBuf::from("/m2/a.jar")];
        let plugins = [PathBuf::from("/m2/plugin.jar")];
        let sources = [PathBuf::from("/p/src/a.kt")];
        let compilation = Compilation {
            release: Some(21),
            jdk_home: Path::new("/jdk"),
            classpath: &classpath,
            module_name: "app",
            java_parameters: true,
            plugins: &plugins,
            kotlin_version: "2.4.20",
            output: Path::new("/out/app"),
            sources: &sources,
        };
        assert_eq!(
            arguments(&compilation).unwrap(),
            [
                "-Xjdk-release=21",
                "-jdk-home=/jdk",
                "-classpath=/out/lib:/m2/a.jar",
                "-no-stdlib",
                "-module-name=app",
                "-java-parameters",
                "-Xplugin=/m2/plugin.jar",
                "-Xkotlin-reference-version=2.4.20",
                "-d",
                "/out/app",
                "/p/src/a.kt",
            ]
            .map(OsString::from)
        );
        let bare = Compilation {
            release: None,
            java_parameters: false,
            plugins: &[],
            ..compilation
        };
        assert_eq!(
            arguments(&bare).unwrap()[..2],
            ["-jdk-home=/jdk", "-classpath=/out/lib:/m2/a.jar"].map(OsString::from)
        );
    }
}
