//! A kotlinc-native oracle for an exact multimodule KLIB graph.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::compilation_target::CompilationTarget;
use krusty::fx_hash::{HashMap, HashSet};
use krusty::kotlin_version::KotlinVersion;

use super::reference_recording_support::{
    kotlin_native_program_arguments, kotlin_native_root, recorded, run_program_bounded,
};

/// What kotlinc-native makes of an exact `// MODULE:` graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeModulesOutcome {
    /// The final module's compilation fails; the compiler's own report.
    Rejected(String),
    /// `box()`'s answer.
    Answer(String),
}

/// What kotlinc-native makes of an exact `// MODULE:` graph built as KLIB dependencies and a final
/// native program. `None` when kotlin-native is not provisioned and no recording exists.
///
/// Every non-final [`krusty::conformance::ModuleUnit`] is a real KLIB and must build. The final
/// unit sees only its declared dependency closure, with declared friends also passed through the
/// Native compiler's friend-module channel; if it fails, the failure is the outcome, recorded like
/// an answer. A folded `dependsOn` chain remains an HMPP compilation: its source files are
/// assigned to their original fragments and its refinement edges are preserved.
pub fn kotlin_native_multimodule_outcome(source: &str) -> Option<NativeModulesOutcome> {
    let recorded_source = [("modules.kt", source)];
    let fingerprint_arguments = ["native-module-graph-v8-joint-fragments".to_string()];
    let files = recorded(
        CompilationTarget::Native,
        "multimodule-program",
        &recorded_source,
        &fingerprint_arguments,
        |work| kotlin_native_multimodule_live(work, source),
    )?;
    if let Some(reason) = files.get("rejected") {
        return Some(NativeModulesOutcome::Rejected(
            String::from_utf8(reason.clone()).expect("recorded text is UTF-8"),
        ));
    }
    let stdout = files
        .get("stdout")
        .expect("a multimodule box recording holds the program's stdout");
    let stdout = String::from_utf8(stdout.clone()).expect("recorded text is UTF-8");
    let (_, answer) = stdout
        .split_once(krusty::backend::BOX_RESULT_FRAME)
        .expect("a multimodule box program prints its framed answer");
    assert!(
        !answer.contains(krusty::backend::BOX_RESULT_FRAME),
        "a multimodule box program prints exactly one result frame"
    );
    Some(NativeModulesOutcome::Answer(answer.to_string()))
}

#[derive(Clone)]
struct BuiltNativeModule {
    klib: PathBuf,
    dependency_closure: Vec<PathBuf>,
}

/// Prepare the whole case before splitting so every module sees the Native placeholder
/// expansion and the same case-wide language/API/opt-in configuration as the target compile.
fn native_module_sources(source: &str) -> (Vec<krusty::conformance::ModuleBlock>, Vec<String>) {
    let prepared =
        krusty::conformance::prepare_test_source(source, krusty::conformance::TestTarget::Native);
    let configuration = krusty::conformance::platform_compiler_configuration(
        &prepared,
        krusty::conformance::TestTarget::Native,
    )
    .expect("Native module directives select a supported compiler configuration");
    let modules = krusty::conformance::split_modules(&prepared)
        .expect("the kotlinc-native multimodule oracle requires a valid // MODULE: graph");
    (modules, configuration.arguments)
}

fn kotlin_native_multimodule_live(work: &Path, source: &str) -> Option<BTreeMap<String, Vec<u8>>> {
    let native_root = kotlin_native_root()?;
    let compiler = native_root.join("bin/kotlinc-native");
    let (modules, compiler_arguments) = native_module_sources(source);
    let units = krusty::conformance::module_units(&modules);
    assert!(
        !units.is_empty(),
        "the kotlinc-native multimodule oracle requires a built unit"
    );

    let final_index = units.len() - 1;
    let mut built = HashMap::<String, BuiltNativeModule>::default();
    for (index, unit) in units.iter().enumerate() {
        assert!(
            unit.java_files.is_empty(),
            "kotlinc-native module {} contains Java sources",
            unit.name
        );
        let final_unit = index == final_index;
        let unit_root = work.join(format!("module-{index:04}"));
        let (source_paths, runner_path) =
            write_native_module_sources(&unit_root, &unit.files, final_unit);

        let mut libraries = Vec::new();
        let mut seen_libraries = HashSet::default();
        for dependency in &unit.deps {
            let dependency = built.get(dependency).unwrap_or_else(|| {
                panic!(
                    "kotlinc-native module {} dependency {dependency} was not built",
                    unit.name
                )
            });
            if seen_libraries.insert(dependency.klib.clone()) {
                libraries.push(dependency.klib.clone());
            }
            for transitive in &dependency.dependency_closure {
                if seen_libraries.insert(transitive.clone()) {
                    libraries.push(transitive.clone());
                }
            }
        }

        let friend_paths = unit
            .friends
            .iter()
            .map(|friend| {
                built
                    .get(friend)
                    .unwrap_or_else(|| {
                        panic!(
                            "kotlinc-native module {} friend {friend} was not built",
                            unit.name
                        )
                    })
                    .klib
                    .clone()
            })
            .collect::<Vec<_>>();
        // Kotlin/Native coalesces equal internal linker symbols from distinct KLIBs by library
        // order even though frontend selection correctly admits only the friend. Releases before
        // 2.4.20 and from 2.5.0-RC on select the first definition while 2.4.20 and 2.4.21
        // select the last, so put the friend at the precedence position used by the pinned
        // compiler.
        order_libraries_for_native_linker(
            &mut libraries,
            &friend_paths,
            krusty::kotlin_version::environment_release(),
        );

        // Only the arguments are used: the persistent server runs the compiler.
        let mut command = Command::new(&compiler);
        command.args(&compiler_arguments);
        command.args(&source_paths);
        if let Some(runner_path) = &runner_path {
            command.arg(runner_path);
        }
        command.args(["-module-name", unit.name.as_str()]);
        // Each module compiles under the graph's `// LANGUAGE:` directives, as the reference
        // runner compiles every module of the test.
        command.args(crate::common::language_directives::kotlinc_args(source));
        for library in &libraries {
            command.arg("-library").arg(library);
        }
        if !friend_paths.is_empty() {
            command.arg("-friend-modules").arg(
                std::env::join_paths(&friend_paths)
                    .expect("kotlinc-native friend KLIB paths are representable"),
            );
        }
        native_fragment_arguments(&mut command, unit, &source_paths, runner_path.as_deref());

        let klib = unit_root.join(format!("{}.klib", unit.name));
        if final_unit {
            command.args(kotlin_native_program_arguments(&native_root));
            command.args(["-produce", "program", "-output"]);
            command.arg(work.join("program"));
        } else {
            command.args(["-produce", "library", "-output"]).arg(&klib);
        }
        let arguments = command.get_args().map(OsString::from).collect::<Vec<_>>();
        let output = super::common::kotlin_native_server::kotlinc_native(&native_root, &arguments);
        if final_unit && output.status != 0 {
            // Only a failure the compiler itself reports is its rejection of the program. A crash
            // or a refused launch without such a line is an environment failure, and recording it
            // would cache it as the oracle's answer.
            if let Some(reason) = output
                .stderr
                .lines()
                .find(|line| line.starts_with("exception:") || line.starts_with("error:"))
            {
                return Some(BTreeMap::from([(
                    "rejected".to_string(),
                    reason.as_bytes().to_vec(),
                )]));
            }
        }
        assert!(
            output.status == 0,
            "kotlinc-native rejected module {} (status {}):\n{}",
            unit.name,
            output.status,
            output.stderr
        );
        if !final_unit {
            assert!(
                klib.is_file(),
                "kotlinc-native accepted module {} but wrote no KLIB at {}",
                unit.name,
                klib.display()
            );
            built.insert(
                unit.name.clone(),
                BuiltNativeModule {
                    klib,
                    dependency_closure: libraries,
                },
            );
        }
    }

    let executable = work.join("program.kexe");
    let run = run_program_bounded(&executable);
    assert!(
        run.status.success(),
        "the kotlinc-native multimodule program exited with {}:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    Some(BTreeMap::from([("stdout".to_string(), run.stdout)]))
}

fn order_libraries_for_native_linker(
    libraries: &mut [PathBuf],
    friends: &[PathBuf],
    version: KotlinVersion,
) {
    if (KotlinVersion::V2_4_20..KotlinVersion::V2_5_0_RC2).contains(&version) {
        libraries.sort_by_key(|library| friends.contains(library));
    } else {
        libraries.sort_by_key(|library| !friends.contains(library));
    }
}

/// Select the callable the final module's generated runner can invoke. Libraries are not in this
/// source set, and only parsed top-level declarations can supply the box entry.
fn box_call_in_final_unit(sources: &[(String, String)]) -> Result<String, &'static str> {
    let named = sources
        .iter()
        .map(|(name, source)| (name.as_str(), source.as_str()))
        .collect::<Vec<_>>();
    let mut diags = krusty::diag::DiagSink::new();
    let files = super::common::parse_source_set_named(&named, &mut diags)
        .ok_or("cannot parse the final Native module's box sources")?;
    let mut selected = None;
    for file in files {
        for &declaration in &file.decls {
            let krusty::ast::Decl::Fun(function) = file.decl(declaration) else {
                continue;
            };
            if function.name != "box"
                || !function.params.is_empty()
                || function.receiver.is_some()
                || !function.type_params.is_empty()
                || function.context_count != 0
                || function.is_suspend()
                || function.visibility.is_private()
                || matches!(function.body, krusty::ast::FunBody::None)
                || file
                    .expect_decls
                    .iter()
                    .any(|header| header.declaration == declaration)
            {
                continue;
            }
            if selected.is_some() {
                return Err(
                    "the final Native module has multiple executable top-level box() entries",
                );
            }
            // The runner stays in the root package so its main entry is unchanged. Quote the
            // parsed package components when rendering a Kotlin call, including escaped keywords.
            selected = Some(match &file.package {
                Some(package) => format!(
                    "{}.box",
                    package
                        .split('.')
                        .map(|component| format!("`{component}`"))
                        .collect::<Vec<_>>()
                        .join(".")
                ),
                None => "box".to_string(),
            });
        }
    }
    selected.ok_or("the final Native module has no executable top-level box() entry")
}

fn write_native_module_sources(
    unit_root: &Path,
    sources: &[(String, String)],
    with_runner: bool,
) -> (Vec<PathBuf>, Option<PathBuf>) {
    let paths = sources
        .iter()
        .enumerate()
        .map(|(index, (name, source))| {
            let root = unit_root.join(format!("source-{index:04}"));
            let leaf = name.rsplit('/').next().unwrap_or(name);
            let path = root.join(format!("{leaf}.kt"));
            std::fs::create_dir_all(&root).expect("create kotlinc-native module source directory");
            std::fs::write(&path, source).expect("write kotlinc-native module source");
            path
        })
        .collect::<Vec<_>>();
    let runner = with_runner.then(|| {
        let path = unit_root.join("KrustyNativeBoxRunner.kt");
        std::fs::create_dir_all(unit_root).expect("create final kotlinc-native module directory");
        let call = box_call_in_final_unit(sources).expect("select the final Native box entry");
        let frame = krusty::backend::BOX_RESULT_FRAME.replace('\0', "\\u0000");
        std::fs::write(
            &path,
            format!("fun main() {{ val answer = {call}(); print(\"{frame}\"); print(answer) }}\n"),
        )
        .expect("write kotlinc-native box runner");
        path
    });
    (paths, runner)
}

fn native_fragment_arguments(
    command: &mut Command,
    unit: &krusty::conformance::ModuleUnit,
    source_paths: &[PathBuf],
    runner_path: Option<&Path>,
) {
    if unit.fragments.len() <= 1 {
        return;
    }
    // kotlinc's own Native box runner compiles a unit's fragments together: it names each fragment,
    // its sources and what it refines, and every fragment sees the unit's `-library` KLIBs. It never
    // passes `-Xseparate-kmp-compilation`, so neither does the oracle.
    command.arg("-Xmulti-platform");
    let mut offset = 0;
    for (index, fragment) in unit.fragments.iter().enumerate() {
        command.arg(format!("-Xfragments={}", fragment.name));
        for path in &source_paths[offset..offset + fragment.kotlin_file_count] {
            command.arg(format!(
                "-Xfragment-sources={}:{}",
                fragment.name,
                path.to_string_lossy()
            ));
        }
        offset += fragment.kotlin_file_count;
        if index + 1 == unit.fragments.len() {
            if let Some(path) = runner_path {
                command.arg(format!(
                    "-Xfragment-sources={}:{}",
                    fragment.name,
                    path.to_string_lossy()
                ));
            }
        }
        for refined in &fragment.refines {
            command.arg(format!("-Xfragment-refines={}:{refined}", fragment.name));
        }
    }
    assert_eq!(
        offset,
        source_paths.len(),
        "every kotlinc-native module source belongs to one HMPP fragment"
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    // PLATFORM-SPECIFIC: Native oracle configuration and prepared module inputs, not JVM ABI.
    #[test]
    fn every_native_module_receives_prepared_sources_and_case_language_settings() {
        let source = "// LANGUAGE: +CompanionBlocks\n\
                      // JVM_TARGET: 1.8\n\
                      // MODULE: lib\n// FILE: W.kt\n\
                      OPTIONAL_JVM_INLINE_ANNOTATION\n\
                      value class W(val value: Int) { companion { fun answer() = 1 } }\n\
                      // MODULE: main(lib)\n// FILE: Main.kt\n\
                      fun box() = if (W.answer() == 1) \"OK\" else \"fail\"\n";
        let (modules, arguments) = native_module_sources(source);
        let expected = vec![
            krusty::conformance::ModuleBlock {
                name: "lib".to_owned(),
                deps: Vec::new(),
                friends: Vec::new(),
                depends_on: Vec::new(),
                files: vec![(
                    "W".to_owned(),
                    "// LANGUAGE: +CompanionBlocks\n// JVM_TARGET: 1.8\n\n\
                     // FILE: W.kt\n\n\
                     value class W(val value: Int) { companion { fun answer() = 1 } }\n"
                        .to_owned(),
                )],
                java_files: Vec::new(),
            },
            krusty::conformance::ModuleBlock {
                name: "main".to_owned(),
                deps: vec!["lib".to_owned()],
                friends: Vec::new(),
                depends_on: Vec::new(),
                files: vec![(
                    "Main".to_owned(),
                    format!(
                        "{}// FILE: Main.kt\nfun box() = if (W.answer() == 1) \"OK\" else \"fail\"\n",
                        "\n".repeat(7)
                    ),
                )],
                java_files: Vec::new(),
            },
        ];
        assert_eq!(modules, expected);
        let flag = krusty::features::FeatureTable::for_version(
            krusty::kotlin_version::environment_release(),
        )
        .expect("the pinned feature table")
        .language_argument("+CompanionBlocks");
        assert_eq!(arguments, vec![flag]);
    }

    #[test]
    fn the_runner_calls_the_box_in_its_declaring_final_unit_file() {
        let source = include_str!("../box_regressions/multimodule/packagedFinalModuleBox.kt");
        let modules =
            krusty::conformance::split_modules(source).expect("valid packaged module graph");
        let unit = krusty::conformance::module_units(&modules)
            .pop()
            .expect("the main module is built last");
        let root = super::super::common::scratch_dir().expect("private runner test directory");
        let (_, runner) = write_native_module_sources(&root, &unit.files, true);
        let runner = runner.expect("the final unit gets a runner");
        let frame = krusty::backend::BOX_RESULT_FRAME.replace('\0', "\\u0000");
        assert_eq!(
            std::fs::read_to_string(runner).expect("read generated runner"),
            format!(
                "fun main() {{ val answer = `packageentry`.`when`.box(); print(\"{frame}\"); print(answer) }}\n"
            )
        );
    }

    #[test]
    fn a_helper_package_and_non_entry_boxes_do_not_replace_a_root_box() {
        let sources = [
            (
                "Members.kt",
                "package members\nclass Holder { fun box() = \"member\" }",
            ),
            (
                "Extension.kt",
                "package extensions\nfun String.box() = this",
            ),
            (
                "Private.kt",
                "package hidden\nprivate fun box() = \"private\"",
            ),
            ("Generic.kt", "package generic\nfun <T> box() = \"generic\""),
            (
                "Suspend.kt",
                "package suspended\nsuspend fun box() = \"suspend\"",
            ),
            ("Main.kt", "fun box() = \"OK\""),
        ]
        .map(|(name, source)| (name.to_string(), source.to_string()));
        assert_eq!(box_call_in_final_unit(&sources), Ok("box".to_string()));
    }

    #[test]
    fn a_refined_actual_box_is_selected_instead_of_its_expect_header() {
        let sources = [
            ("Common.kt", "package refinement\nexpect fun box(): String"),
            ("Main.kt", "package refinement\nactual fun box() = \"OK\""),
        ]
        .map(|(name, source)| (name.to_string(), source.to_string()));
        assert_eq!(
            box_call_in_final_unit(&sources),
            Ok("`refinement`.box".to_string())
        );
    }

    #[test]
    fn missing_and_ambiguous_final_boxes_are_rejected_without_a_package_fallback() {
        let missing = [("Main.kt".to_string(), "fun helper() = \"OK\"".to_string())];
        assert_eq!(
            box_call_in_final_unit(&missing),
            Err("the final Native module has no executable top-level box() entry")
        );
        let ambiguous = [
            ("First.kt", "package first\nfun box() = \"first\""),
            ("Second.kt", "package second\nfun box() = \"second\""),
        ]
        .map(|(name, source)| (name.to_string(), source.to_string()));
        assert_eq!(
            box_call_in_final_unit(&ambiguous),
            Err("the final Native module has multiple executable top-level box() entries")
        );
    }

    #[test]
    fn friend_library_precedence_follows_the_native_linker_version() {
        let friend = PathBuf::from("/work/friend.klib");
        let sibling = PathBuf::from("/work/sibling.klib");
        let transitive = PathBuf::from("/work/transitive.klib");
        let other = PathBuf::from("/work/other.klib");
        let original = vec![
            friend.clone(),
            sibling.clone(),
            transitive.clone(),
            other.clone(),
        ];

        let mut before_2_4_20 = original.clone();
        order_libraries_for_native_linker(
            &mut before_2_4_20,
            std::slice::from_ref(&friend),
            KotlinVersion::V2_4_10,
        );
        assert_eq!(before_2_4_20, original);

        let mut since_2_4_20 = original;
        order_libraries_for_native_linker(
            &mut since_2_4_20,
            std::slice::from_ref(&friend),
            KotlinVersion::V2_4_20,
        );
        assert_eq!(
            since_2_4_20,
            [
                sibling.clone(),
                transitive.clone(),
                other.clone(),
                friend.clone()
            ]
        );

        let original = vec![friend.clone(), sibling, transitive, other];
        let mut since_2_5 = original.clone();
        order_libraries_for_native_linker(
            &mut since_2_5,
            std::slice::from_ref(&friend),
            KotlinVersion::V2_5_0_RC2,
        );
        assert_eq!(since_2_5, original);
    }

    #[test]
    fn a_unit_compiles_its_fragments_together_like_kotlincs_native_runner() {
        let modules = krusty::conformance::split_modules(
            "// MODULE: common\nexpect fun answer(): String\n\
             // MODULE: main()()(common)\nactual fun answer() = \"OK\"\n",
        )
        .expect("test graph is valid");
        let unit = krusty::conformance::module_units(&modules)
            .pop()
            .expect("the platform unit folds its common fragment");
        let source_paths = [
            PathBuf::from("/work/common.kt"),
            PathBuf::from("/work/main.kt"),
        ];
        let runner = Path::new("/work/KrustyNativeBoxRunner.kt");
        let mut command = Command::new("kotlinc-native");

        native_fragment_arguments(&mut command, &unit, &source_paths, Some(runner));

        let arguments = command.get_args().collect::<Vec<&OsStr>>();
        assert_eq!(
            arguments,
            [
                "-Xmulti-platform",
                "-Xfragments=common",
                "-Xfragment-sources=common:/work/common.kt",
                "-Xfragments=main",
                "-Xfragment-sources=main:/work/main.kt",
                "-Xfragment-sources=main:/work/KrustyNativeBoxRunner.kt",
                "-Xfragment-refines=main:common",
            ]
            .map(OsStr::new)
        );
    }
}
