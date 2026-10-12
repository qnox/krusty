//! A kotlinc-native oracle for an exact multimodule KLIB graph.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use krusty::compilation_target::CompilationTarget;
use krusty::fx_hash::{HashMap, HashSet};
use krusty::kotlin_version::KotlinVersion;

use super::reference_recording_support::{kotlin_native_root, recorded};

/// What `box()` answers when kotlinc-native builds an exact `// MODULE:` graph as KLIB
/// dependencies and a final native program. `None` when kotlin-native is not provisioned and no
/// recording exists.
///
/// Every non-final [`krusty::conformance::ModuleUnit`] is a real KLIB. The final unit sees only its
/// declared dependency closure, with declared friends also passed through the Native compiler's
/// friend-module channel. A folded `dependsOn` chain remains an HMPP compilation: its source files
/// are assigned to their original fragments and its refinement edges are preserved.
pub fn kotlin_native_multimodule_box(source: &str) -> Option<String> {
    let recorded_source = [("modules.kt", source)];
    let fingerprint_arguments = ["native-module-graph-v4-versioned-friend-precedence".to_string()];
    let files = recorded(
        CompilationTarget::Native,
        "multimodule-program",
        &recorded_source,
        &fingerprint_arguments,
        |work| kotlin_native_multimodule_live(work, source),
    )?;
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
    Some(answer.to_string())
}

#[derive(Clone)]
struct BuiltNativeModule {
    klib: PathBuf,
    dependency_closure: Vec<PathBuf>,
}

fn kotlin_native_multimodule_live(work: &Path, source: &str) -> Option<BTreeMap<String, Vec<u8>>> {
    let native_root = kotlin_native_root()?;
    let compiler = native_root.join("bin/kotlinc-native");
    let stdlib = native_root.join("klib/common/stdlib");
    assert!(
        stdlib.is_dir(),
        "the Kotlin/Native distribution has no stdlib KLIB at {}",
        stdlib.display()
    );
    let modules = krusty::conformance::split_modules(source)
        .expect("the kotlinc-native multimodule oracle requires a valid // MODULE: graph");
    let units = krusty::conformance::module_units(&modules);
    assert!(
        !units.is_empty(),
        "the kotlinc-native multimodule oracle requires a built unit"
    );

    // `module_units` assigns a folded source set to its first built representative. Reconstruct
    // that same name map so a non-leaf HMPP fragment's own dependency/friend declarations can name
    // the exact KLIB output which the production graph assigned to them.
    let mut representatives = HashMap::<String, String>::default();
    for unit in &units {
        for fragment in &unit.fragments {
            representatives
                .entry(fragment.name.clone())
                .or_insert_with(|| unit.name.clone());
        }
    }
    let module_blocks = modules
        .iter()
        .map(|module| (module.name.as_str(), module))
        .collect::<HashMap<_, _>>();

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
        // 2.4.20 select the first definition while 2.4.20 selects the last, so put the friend at
        // the precedence position used by the pinned compiler.
        order_libraries_for_native_linker(
            &mut libraries,
            &friend_paths,
            krusty::kotlin_version::target(),
        );

        let mut command = Command::new(&compiler);
        command.args(&source_paths);
        if let Some(runner_path) = &runner_path {
            command.arg(runner_path);
        }
        // Separate KMP compilation cannot attach kotlinc-native's implicit stdlib to common
        // fragments. Supply the same KLIB explicitly so the platform leaf receives it through
        // `-library` and each non-leaf can name it through `-Xfragment-dependency` below.
        command.arg("-nostdlib").arg("-library").arg(&stdlib);
        command.args(["-module-name", unit.name.as_str()]);
        for library in &libraries {
            command.arg("-library").arg(library);
        }
        if !friend_paths.is_empty() {
            command.arg("-friend-modules").arg(
                std::env::join_paths(&friend_paths)
                    .expect("kotlinc-native friend KLIB paths are representable"),
            );
        }
        native_fragment_arguments(
            &mut command,
            unit,
            &source_paths,
            runner_path.as_deref(),
            &module_blocks,
            &representatives,
            &built,
            &stdlib,
        );

        let klib = unit_root.join(format!("{}.klib", unit.name));
        if final_unit {
            command.args(["-produce", "program", "-output"]);
            command.arg(work.join("program"));
        } else {
            command.args(["-produce", "library", "-output"]).arg(&klib);
        }
        let output = command
            .output()
            .expect("run kotlinc-native module compiler");
        assert!(
            output.status.success(),
            "kotlinc-native rejected module {} (status {}):\n{}",
            unit.name,
            output.status,
            String::from_utf8_lossy(&output.stderr)
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
    let run = Command::new(&executable)
        .output()
        .expect("run the kotlinc-native multimodule program");
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
    if version >= KotlinVersion::V2_4_20 {
        libraries.sort_by_key(|library| friends.contains(library));
    } else {
        libraries.sort_by_key(|library| !friends.contains(library));
    }
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
        let frame = krusty::backend::BOX_RESULT_FRAME.replace('\0', "\\u0000");
        std::fs::write(
            &path,
            format!("fun main() {{ val answer = box(); print(\"{frame}\"); print(answer) }}\n"),
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
    module_blocks: &HashMap<&str, &krusty::conformance::ModuleBlock>,
    representatives: &HashMap<String, String>,
    built: &HashMap<String, BuiltNativeModule>,
    stdlib: &Path,
) {
    if unit.fragments.len() <= 1 {
        return;
    }
    command.args(["-Xmulti-platform", "-Xseparate-kmp-compilation"]);
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

        // The platform leaf takes its dependencies from `-library`/`-friend-modules`. Every common
        // fragment must instead identify which of those KLIBs belongs to that fragment.
        if index + 1 == unit.fragments.len() {
            continue;
        }
        command.arg(format!(
            "-Xfragment-dependency={}:{}",
            fragment.name,
            stdlib.to_string_lossy()
        ));
        let block = module_blocks
            .get(fragment.name.as_str())
            .expect("every HMPP fragment came from a parsed module block");
        for (dependencies, argument) in [
            (&block.deps, "-Xfragment-dependency"),
            (&block.friends, "-Xfragment-friend-dependency"),
        ] {
            for dependency in dependencies {
                let representative = representatives
                    .get(dependency)
                    .map(String::as_str)
                    .unwrap_or(dependency);
                if representative == unit.name {
                    continue;
                }
                let klib = &built
                    .get(representative)
                    .unwrap_or_else(|| {
                        panic!(
                            "kotlinc-native fragment {} dependency {representative} was not built",
                            fragment.name
                        )
                    })
                    .klib;
                command.arg(format!(
                    "{argument}={}:{}",
                    fragment.name,
                    klib.to_string_lossy()
                ));
            }
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
        assert_eq!(since_2_4_20, [sibling, transitive, other, friend]);
    }

    #[test]
    fn common_fragments_receive_the_native_stdlib_klib() {
        let modules = krusty::conformance::split_modules(
            "// MODULE: common\nexpect fun answer(): String\n\
             // MODULE: main()()(common)\nactual fun answer() = \"OK\"\n",
        )
        .expect("test graph is valid");
        let unit = krusty::conformance::module_units(&modules)
            .pop()
            .expect("the platform unit folds its common fragment");
        let module_blocks = modules
            .iter()
            .map(|block| (block.name.as_str(), block))
            .collect::<HashMap<_, _>>();
        let representatives = unit
            .fragments
            .iter()
            .map(|fragment| (fragment.name.clone(), unit.name.clone()))
            .collect::<HashMap<_, _>>();
        let source_paths = [
            PathBuf::from("/work/common.kt"),
            PathBuf::from("/work/main.kt"),
        ];
        let runner = Path::new("/work/KrustyNativeBoxRunner.kt");
        let stdlib = Path::new("/konan/klib/common/stdlib");
        let mut command = Command::new("kotlinc-native");

        native_fragment_arguments(
            &mut command,
            &unit,
            &source_paths,
            Some(runner),
            &module_blocks,
            &representatives,
            &HashMap::default(),
            stdlib,
        );

        let arguments = command.get_args().collect::<Vec<&OsStr>>();
        assert_eq!(
            arguments,
            [
                "-Xmulti-platform",
                "-Xseparate-kmp-compilation",
                "-Xfragments=common",
                "-Xfragment-sources=common:/work/common.kt",
                "-Xfragment-dependency=common:/konan/klib/common/stdlib",
                "-Xfragments=main",
                "-Xfragment-sources=main:/work/main.kt",
                "-Xfragment-sources=main:/work/KrustyNativeBoxRunner.kt",
                "-Xfragment-refines=main:common",
            ]
            .map(OsStr::new)
        );
    }
}
