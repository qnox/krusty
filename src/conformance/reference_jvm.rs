//! Ordinary Kotlin/JVM acceptance oracle for target-gated codegen tests.
//!
//! A `DONT_TARGET_EXACT_BACKEND` directive is not a frontend verdict: many such sources are valid
//! Kotlin/JVM and differ only at runtime or in code generation. When krusty rejects one, the survey
//! invokes the pinned ordinary compiler on the same prepared source set. Only its normal compilation
//! rejection can remove the case from the positive JVM-acceptance denominator.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    directive, inject_support_module, module_units, split_files, split_modules, SourceBlock,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReferenceJvmAcceptance {
    Accepted,
    Rejected,
    Unavailable(String),
}

struct ScratchDir(PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_dir() -> Result<ScratchDir, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "krusty_reference_jvm_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path)
        .map_err(|error| format!("cannot create reference JVM scratch directory: {error}"))?;
    Ok(ScratchDir(path))
}

fn language_args(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|line| line.trim_start().strip_prefix("// LANGUAGE:"))
        .flat_map(|payload| payload.split([' ', ',', '\t']))
        .filter(|token| !token.is_empty())
        .map(|token| format!("-XXLanguage:{token}"))
        .collect()
}

fn write_sources(
    root: &Path,
    kotlin: &[SourceBlock],
    java: &[SourceBlock],
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    fn write_group(
        root: &Path,
        blocks: &[SourceBlock],
        kotlin: bool,
    ) -> Result<Vec<PathBuf>, String> {
        blocks
            .iter()
            .enumerate()
            .map(|(index, (name, source))| {
                let dir = root.join(index.to_string());
                std::fs::create_dir_all(&dir).map_err(|error| {
                    format!("cannot create reference source directory: {error}")
                })?;
                let file = if kotlin {
                    dir.join(format!("{name}.kt"))
                } else {
                    dir.join(name)
                };
                std::fs::write(&file, source)
                    .map_err(|error| format!("cannot write reference source: {error}"))?;
                Ok(file)
            })
            .collect()
    }

    Ok((
        write_group(&root.join("kotlin"), kotlin, true)?,
        write_group(&root.join("java"), java, false)?,
    ))
}

fn compile_unit(
    kotlinc: &Path,
    root: &Path,
    module_name: &str,
    kotlin: &[SourceBlock],
    java: &[SourceBlock],
    common_file_count: usize,
    classpath: &[PathBuf],
    friend_paths: &[PathBuf],
    language_args: &[String],
) -> ReferenceJvmAcceptance {
    let source_root = root.join("sources");
    let output = root.join("classes");
    if let Err(error) = std::fs::create_dir_all(&output) {
        return ReferenceJvmAcceptance::Unavailable(format!(
            "cannot create reference output directory: {error}"
        ));
    }
    if kotlin.is_empty() {
        // There is no Kotlin frontend verdict in a Java-only unit. Its output would require javac
        // orchestration before a dependent module can be graded, which this oracle must not guess.
        return if java.is_empty() {
            ReferenceJvmAcceptance::Accepted
        } else {
            ReferenceJvmAcceptance::Unavailable(
                "reference JVM oracle cannot publish a Java-only dependency unit".into(),
            )
        };
    }
    let (kotlin_paths, java_paths) = match write_sources(&source_root, kotlin, java) {
        Ok(paths) => paths,
        Err(error) => return ReferenceJvmAcceptance::Unavailable(error),
    };

    let mut command = Command::new(kotlinc);
    command
        .args(&kotlin_paths)
        .args(&java_paths)
        .arg("-d")
        .arg(&output)
        .arg("-module-name")
        .arg(module_name)
        .arg("-Xjdk-release=8")
        .args(language_args);
    if !classpath.is_empty() {
        let classpath = match std::env::join_paths(classpath) {
            Ok(classpath) => classpath,
            Err(error) => {
                return ReferenceJvmAcceptance::Unavailable(format!(
                    "invalid reference JVM classpath: {error}"
                ))
            }
        };
        command.arg("-classpath").arg(classpath);
    }
    if !friend_paths.is_empty() {
        let friends = match std::env::join_paths(friend_paths) {
            Ok(friends) => friends,
            Err(error) => {
                return ReferenceJvmAcceptance::Unavailable(format!(
                    "invalid reference JVM friend path: {error}"
                ))
            }
        };
        command.arg(format!("-Xfriend-paths={}", friends.to_string_lossy()));
    }
    if common_file_count != 0 {
        command.arg("-Xmulti-platform");
        let common = kotlin_paths[..common_file_count]
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(",");
        command.arg(format!("-Xcommon-sources={common}"));
    }

    match command.output() {
        Ok(result) if result.status.success() => ReferenceJvmAcceptance::Accepted,
        // Kotlin CLI exit 1 is a normal compilation rejection. Crashes, command-line errors, and
        // signals are harness failures and must never shrink krusty's conformance denominator.
        Ok(result) if result.status.code() == Some(1) => ReferenceJvmAcceptance::Rejected,
        Ok(result) => ReferenceJvmAcceptance::Unavailable(format!(
            "reference JVM compiler exited with {:?}: {}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).trim()
        )),
        Err(error) => ReferenceJvmAcceptance::Unavailable(format!(
            "cannot execute reference JVM compiler: {error}"
        )),
    }
}

/// Ask the pinned ordinary Kotlin/JVM compiler whether it accepts the prepared codegen-test source.
///
/// `coroutine_helpers` is the same generated support source the production harness injects. The
/// caller supplies the exact directive-selected classpath used for krusty's compilation.
pub fn reference_jvm_acceptance(
    src: &str,
    fallback_stem: &str,
    classpath: &[PathBuf],
    coroutine_helpers: &str,
) -> ReferenceJvmAcceptance {
    let Some(kotlinc) = crate::toolchain::kotlinc_path() else {
        return ReferenceJvmAcceptance::Unavailable("reference kotlinc is unavailable".into());
    };
    let scratch = match scratch_dir() {
        Ok(scratch) => scratch,
        Err(error) => return ReferenceJvmAcceptance::Unavailable(error),
    };
    let args = language_args(src);

    if src.contains("// MODULE:") {
        let Some(mut modules) = split_modules(src) else {
            return ReferenceJvmAcceptance::Unavailable(
                "reference JVM oracle does not support this // MODULE: shape".into(),
            );
        };
        if directive(src, "WITH_COROUTINES") {
            inject_support_module(&mut modules, coroutine_helpers);
        }
        let units = module_units(&modules);
        let mut outputs = HashMap::<String, PathBuf>::new();
        for unit in units {
            let mut unit_classpath = classpath.to_vec();
            let mut friends = Vec::new();
            for dependency in &unit.deps {
                let Some(output) = outputs.get(dependency) else {
                    return ReferenceJvmAcceptance::Unavailable(format!(
                        "reference JVM dependency {dependency} was not built"
                    ));
                };
                unit_classpath.push(output.clone());
                if unit.friends.contains(dependency) {
                    friends.push(output.clone());
                }
            }
            let unit_root = scratch.0.join(&unit.name);
            let result = compile_unit(
                &kotlinc,
                &unit_root,
                &unit.name,
                &unit.files,
                &unit.java_files,
                unit.common_file_count,
                &unit_classpath,
                &friends,
                &args,
            );
            if result != ReferenceJvmAcceptance::Accepted {
                return result;
            }
            outputs.insert(unit.name, unit_root.join("classes"));
        }
        return ReferenceJvmAcceptance::Accepted;
    }

    let (mut kotlin, java) = if src.contains("// FILE:") {
        split_files(src)
    } else {
        (
            vec![(fallback_stem.to_string(), src.to_string())],
            Vec::new(),
        )
    };
    if directive(src, "WITH_COROUTINES") {
        kotlin.push(("CoroutineUtil".into(), coroutine_helpers.into()));
    }
    compile_unit(
        &kotlinc,
        &scratch.0.join("main"),
        "main",
        &kotlin,
        &java,
        0,
        classpath,
        &[],
        &args,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_directives_become_exact_compiler_arguments() {
        assert_eq!(
            language_args("// LANGUAGE: +ContextParameters,-Feature\nfun box() = \"OK\""),
            [
                "-XXLanguage:+ContextParameters".to_string(),
                "-XXLanguage:-Feature".to_string(),
            ]
        );
    }
}
