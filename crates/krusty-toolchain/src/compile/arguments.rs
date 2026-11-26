//! The compiler arguments of one module's main fragment, in the order the toolchain passes them to
//! the Kotlin compiler (through the Build Tools API, which krusty receives as a command line).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::sources::Sources;

/// What one compilation is made of, but for its sources.
pub(super) struct Compilation {
    /// `jvm.release`, when set.
    pub release: Option<i32>,
    pub jdk_home: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub module_name: String,
    /// `jvm.storeParameterNames`.
    pub java_parameters: bool,
    /// Compiler plugin jars, in order.
    pub plugins: Vec<PathBuf>,
    /// `kotlin.version`: the Kotlin release krusty compiles as.
    pub kotlin_version: String,
    pub output: PathBuf,
}

/// The name of a module's main fragment.
const MAIN_FRAGMENT: &str = "main";

fn joined(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

/// The arguments of `compilation` over the main fragment's `sources`, the program name excluded.
/// The Kotlin files are passed sorted. A fragment of more than one source directory is described
/// to the compiler (`-Xmulti-platform`, `-Xfragments`, `-Xfragment-sources`) with its files in
/// the order the toolchain walks its source directories.
pub(super) fn arguments(
    compilation: &Compilation,
    sources: &Sources,
) -> Result<Vec<OsString>, String> {
    let mut arguments = Vec::new();
    if let Some(release) = compilation.release {
        arguments.push(OsString::from(format!("-Xjdk-release={release}")));
    }
    arguments.push(joined("-jdk-home=", &compilation.jdk_home));
    let classpath = std::env::join_paths(&compilation.classpath).map_err(|error| {
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
    if sources.filled > 1 {
        arguments.push(OsString::from("-Xmulti-platform"));
        arguments.push(OsString::from(format!("-Xfragments={MAIN_FRAGMENT}")));
        for source in &sources.walked {
            arguments.push(joined(
                &format!("-Xfragment-sources={MAIN_FRAGMENT}:"),
                source,
            ));
        }
    }
    for plugin in &compilation.plugins {
        arguments.push(joined("-Xplugin=", plugin));
    }
    arguments.push(OsString::from(format!(
        "-Xkotlin-reference-version={}",
        compilation.kotlin_version
    )));
    arguments.push(OsString::from("-d"));
    arguments.push(compilation.output.as_os_str().to_owned());
    arguments.extend(
        sources
            .kotlin
            .iter()
            .map(|source| source.as_os_str().to_owned()),
    );
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_arguments_follow_the_toolchain_order() {
        let compilation = Compilation {
            release: Some(21),
            jdk_home: PathBuf::from("/jdk"),
            classpath: paths(&["/out/lib", "/m2/a.jar"]),
            module_name: "app".to_string(),
            java_parameters: true,
            plugins: paths(&["/m2/plugin.jar"]),
            kotlin_version: "2.4.20".to_string(),
            output: PathBuf::from("/out/app"),
        };
        let sources = Sources {
            kotlin: paths(&["/p/src/a.kt"]),
            walked: paths(&["/p/src/a.kt"]),
            filled: 1,
            ..Sources::default()
        };
        assert_eq!(
            arguments(&compilation, &sources).unwrap(),
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
            plugins: Vec::new(),
            ..compilation
        };
        assert_eq!(
            arguments(&bare, &sources).unwrap()[..2],
            ["-jdk-home=/jdk", "-classpath=/out/lib:/m2/a.jar"].map(OsString::from)
        );
    }

    #[test]
    fn a_fragment_of_several_source_directories_is_described_before_the_plugins() {
        let compilation = Compilation {
            release: None,
            jdk_home: PathBuf::from("/jdk"),
            classpath: Vec::new(),
            module_name: "app".to_string(),
            java_parameters: true,
            plugins: paths(&["/m2/plugin.jar"]),
            kotlin_version: "2.4.20".to_string(),
            output: PathBuf::from("/out/app"),
        };
        let sources = Sources {
            kotlin: paths(&["/p/gen/a.kt", "/p/src/b.kt"]),
            walked: paths(&["/p/src/b.kt", "/p/gen/a.kt"]),
            filled: 2,
            ..Sources::default()
        };
        assert_eq!(
            arguments(&compilation, &sources).unwrap(),
            [
                "-jdk-home=/jdk",
                "-classpath=",
                "-no-stdlib",
                "-module-name=app",
                "-java-parameters",
                "-Xmulti-platform",
                "-Xfragments=main",
                "-Xfragment-sources=main:/p/src/b.kt",
                "-Xfragment-sources=main:/p/gen/a.kt",
                "-Xplugin=/m2/plugin.jar",
                "-Xkotlin-reference-version=2.4.20",
                "-d",
                "/out/app",
                "/p/gen/a.kt",
                "/p/src/b.kt",
            ]
            .map(OsString::from)
        );
    }
}
