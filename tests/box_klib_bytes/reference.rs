//! Exact-toolchain reference KLIB entries, cached independently by complete input identity.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use krusty::conformance::{ModuleUnit, TestTarget};

use super::super::common::byte_dump;
use super::{dependencies, BuiltModule, QualifiedArtifacts, Scratch};

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);

pub(super) fn cache_stats() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

pub(super) fn compile(
    target: TestTarget,
    source: &str,
    units: &[ModuleUnit],
) -> Result<(QualifiedArtifacts, bool), String> {
    let toolchain = toolchain(target)?;
    let foreign = if target == TestTarget::Native {
        super::native_interop::Modules::new(source, &toolchain.compiler)?
    } else {
        super::native_interop::Modules::default()
    };
    let identity = format!("{}/{}", toolchain.identity, foreign.identity());
    let fingerprint = key(target, source, units, &identity);
    let path = super::reference_cache::path(fingerprint);
    let cacheable = byte_dump::published_compiler_id().is_some();
    let cached = cacheable
        .then(|| super::reference_cache::load(&path, fingerprint))
        .flatten();
    let (files, replayed) = match cached {
        Some(files) => {
            HITS.fetch_add(1, Ordering::Relaxed);
            (files, true)
        }
        None => {
            MISSES.fetch_add(1, Ordering::Relaxed);
            let files = live(target, source, units, &toolchain, &foreign)?;
            if cacheable {
                super::reference_cache::store(&path, fingerprint, &files)?;
            }
            (files, false)
        }
    };
    if let Some(error) = files.get("@error") {
        return Err(String::from_utf8(error.clone()).map_err(|e| e.to_string())?);
    }
    let mut result = QualifiedArtifacts::new();
    for (name, bytes) in files {
        let (module, entry) = decode_artifact_key(&name)?;
        result.insert((module, entry), bytes);
    }
    if result.is_empty() {
        return Err("reference emitted no KLIB entries".into());
    }
    Ok((result, replayed))
}

/// The reference-artifact format, bumped deliberately whenever how the reference is invoked or
/// how its entries are stored changes. Everything a compile reads already rides the key: the
/// target, architecture, source, module plan, derived compiler configuration and properties,
/// directive-selected KLIB libraries, and the toolchain identity. The harness's own sources are
/// deliberately not hashed: an unrelated
/// edit to one of them would turn every Native and Wasm row cold. CI hashes the same file into
/// its cache key, so a bump starts a fresh cache instead of saving new archives beside the old.
const REFERENCE_FORMAT: &str = include_str!("reference-format");

pub(super) fn key(target: TestTarget, source: &str, units: &[ModuleUnit], identity: &str) -> u128 {
    // ModuleUnit's full topology includes folded fragments, friend policy, source names and bytes.
    let plan = format!("{units:?}");
    let configuration = krusty::conformance::platform_compiler_configuration(source, target);
    let properties = configuration.as_ref().map(|configuration| {
        krusty::conformance::reference_jvm_properties(
            configuration.arguments.iter().map(String::as_str),
        )
    });
    // Directive libraries ride the key only when a case selects some, so an ordinary case keeps
    // the fingerprint it had before those libraries were passed to the reference.
    let mut settings = format!("{configuration:?}/{properties:?}");
    if let Ok(libraries) =
        krusty::toolchain::klib_libraries_for(source, target.compilation_target())
    {
        for library in libraries {
            settings.push('\0');
            settings.push_str(&library.to_string_lossy());
        }
    }
    byte_dump::fingerprint_parts(&[
        REFERENCE_FORMAT.as_bytes(),
        format!("{target:?}").as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        source.as_bytes(),
        plan.as_bytes(),
        settings.as_bytes(),
        identity.as_bytes(),
    ])
}

// Cache paths encode module bytes separately from the artifact path: a slash or Unicode in a
// source module identity must never be reinterpreted as another module/entry pair.
fn artifact_key(module: &str, entry: &str) -> String {
    use std::fmt::Write as _;
    let mut key = String::new();
    for byte in module.bytes() {
        write!(&mut key, "{byte:02x}").expect("write a String");
    }
    key.push('/');
    key.push_str(entry);
    key
}

fn decode_artifact_key(key: &str) -> Result<(String, String), String> {
    let invalid = || format!("invalid cached KLIB identity {key:?}");
    let (module, entry) = key.split_once('/').ok_or_else(invalid)?;
    if module.is_empty() || module.len() % 2 != 0 || !module.is_ascii() {
        return Err(invalid());
    }
    let bytes = (0..module.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&module[index..index + 2], 16).map_err(|_| invalid()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        String::from_utf8(bytes).map_err(|_| invalid())?,
        entry.to_owned(),
    ))
}

struct Toolchain {
    compiler: PathBuf,
    stdlib: PathBuf,
    test_library: Option<PathBuf>,
    identity: String,
}
fn toolchain(target: TestTarget) -> Result<Toolchain, String> {
    let lib = krusty::toolchain::kotlinc_lib_dir().ok_or("no selected kotlinc distribution")?;
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(target.compilation_target())
        .ok_or("no target stdlib KLIB")?;
    let (compiler, root) = if target == TestTarget::Native {
        let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
            .map(PathBuf::from)
            .ok_or("no exact Native distribution")?;
        let properties = std::fs::read_to_string(root.join("konan/konan.properties"))
            .map_err(|error| format!("read Native compiler identity: {error}"))?;
        let version = properties
            .lines()
            .find_map(|line| line.strip_prefix("compilerVersion="))
            .ok_or("Native distribution has no compilerVersion")?;
        let selected = krusty::kotlin_version::target().to_string();
        if version != selected {
            return Err(format!(
                "selected Native compiler {selected}, distribution reports {version}"
            ));
        }
        (root.join("bin/kotlinc-native"), root)
    } else {
        (lib.with_file_name("bin").join("kotlinc-wasm"), lib.clone())
    };
    let test_library = match target {
        TestTarget::WasmJs | TestTarget::WasmWasi => {
            let artifact = if target == TestTarget::WasmJs {
                "kotlin-test-wasm-js"
            } else {
                "kotlin-test-wasm-wasi"
            };
            Some(
                krusty::toolchain::ensure_maven_artifact(
                    "org.jetbrains.kotlin",
                    artifact,
                    &krusty::kotlin_version::target().to_string(),
                    "klib",
                )
                .ok_or("no exact target kotlin-test KLIB")?,
            )
        }
        _ => None,
    };
    let mut inputs = vec![root, compiler.clone(), stdlib.clone()];
    inputs.extend(test_library.iter().cloned());
    // Immutable toolchain content, including target dependencies and producing JDK, is hashed once.
    static IDENTITIES: OnceLock<std::sync::Mutex<BTreeMap<Vec<PathBuf>, String>>> = OnceLock::new();
    let mut identities = IDENTITIES.get_or_init(Default::default).lock().unwrap();
    let identity = if let Some(identity) = identities.get(&inputs) {
        identity.clone()
    } else {
        let modules = krusty::toolchain::jdk_modules().ok_or("no producing JDK modules")?;
        let identity = byte_dump::immutable_classpath_content_fingerprint(&inputs, &modules)?;
        identities.insert(inputs, identity.clone());
        identity
    };
    Ok(Toolchain {
        compiler,
        stdlib,
        test_library,
        identity,
    })
}

fn live(
    target: TestTarget,
    source: &str,
    units: &[ModuleUnit],
    toolchain: &Toolchain,
    foreign: &super::native_interop::Modules,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let units = reference_units(source, units, target)?;
    let scratch = Scratch::new()?;
    let graph = FragmentGraph::new(source, target, &units)?;
    let directive_libraries =
        krusty::toolchain::klib_libraries_for(source, target.compilation_target())?;
    let mut built = BTreeMap::new();
    let mut inventory = BTreeMap::new();
    for (ordinal, unit) in units.iter().enumerate() {
        if !unit.java_files.is_empty() {
            return Err(format!("KLIB module {} contains Java sources", unit.name));
        }
        let (mut libraries, friends) = dependencies(unit, &built)?;
        if foreign.contains(&unit.name) {
            let archive = foreign.compile(
                &toolchain.compiler,
                source,
                &unit.name,
                &scratch.0,
                &libraries,
            )?;
            collect_module_entries(&mut inventory, &unit.name, &archive)?;
            built.insert(
                unit.name.clone(),
                BuiltModule {
                    path: archive,
                    dependencies: libraries,
                },
            );
            continue;
        }
        for library in &directive_libraries {
            if !libraries.contains(library) {
                libraries.push(library.clone());
            }
        }
        let work = scratch.0.join(format!("module-{ordinal}"));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut paths = Vec::new();
        for (index, (stem, text)) in unit.files.iter().enumerate() {
            let path = PathBuf::from(format!(
                "source-{index}/{}.kt",
                stem.rsplit('/').next().unwrap_or(stem)
            ));
            std::fs::create_dir_all(work.join(path.parent().unwrap()))
                .map_err(|e| e.to_string())?;
            std::fs::write(work.join(&path), text).map_err(|e| e.to_string())?;
            paths.push(work.join(path));
        }
        let archive = work.join("output");
        let mut command = command(target, toolchain, &work, &unit.name)?;
        if target == TestTarget::Native {
            command.arg("-library").arg(&toolchain.stdlib);
            for library in &libraries {
                command.arg("-library").arg(library);
            }
        } else {
            let paths = std::iter::once(&toolchain.stdlib)
                .chain(toolchain.test_library.iter())
                .chain(&libraries);
            command
                .arg("-libraries")
                .arg(std::env::join_paths(paths).map_err(|e| e.to_string())?);
        }
        if !friends.is_empty() {
            let paths = std::env::join_paths(&friends).map_err(|e| e.to_string())?;
            if target == TestTarget::Native {
                command.arg("-friend-modules").arg(paths);
            } else {
                command.arg(format!(
                    "-Xfriend-modules={}",
                    paths.to_str().ok_or("non-UTF8 friend path")?
                ));
            }
        }
        fragment_arguments(
            &mut command,
            unit,
            &paths,
            &built,
            &toolchain.stdlib,
            &directive_libraries,
            &graph,
        )?;
        let configuration = krusty::conformance::platform_compiler_configuration(source, target)?;
        command.args(&configuration.arguments);
        command.args(krusty::conformance::reference_jvm_properties(
            configuration.arguments.iter().map(String::as_str),
        ));
        command.args(
            krusty::conformance::reference_only_kotlinc_arguments_for_target(source, target)?,
        );

        command.args(&paths);
        let arguments = command
            .get_args()
            .map(|arg| {
                arg.to_str()
                    .map(str::to_owned)
                    .ok_or("non-UTF8 compiler argument".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let language = configuration
            .language_version
            .map(|version| version.to_string());
        let api = configuration.api_version.map(|version| version.to_string());
        let (status, diagnostics) =
            super::super::common::kotlinc_server::kotlinc_compile_for_target(
                target.compilation_target(),
                &arguments,
                language.as_deref(),
                api.as_deref(),
            )
            .ok_or("exact-target persistent reference compiler unavailable")?;
        if status != 0 {
            let message = format!(
                "reference rejected module {} (status {status}):\n{diagnostics}",
                unit.name
            );
            if status == 1
                && !super::super::common::reference_error_blocks_from(&diagnostics).is_empty()
            {
                return Ok(BTreeMap::from([("@error".into(), message.into_bytes())]));
            }
            return Err(message);
        }
        collect_module_entries(&mut inventory, &unit.name, &archive)?;
        built.insert(
            unit.name.clone(),
            BuiltModule {
                path: archive,
                dependencies: libraries,
            },
        );
    }
    Ok(inventory)
}

fn collect_module_entries(
    inventory: &mut BTreeMap<String, Vec<u8>>,
    module: &str,
    archive: &Path,
) -> Result<(), String> {
    let klib = krusty::klib::KlibArchive::open(archive)
        .map_err(|error| format!("read reference module {module}: {error}"))?;
    for entry in klib.entries() {
        inventory.insert(
            artifact_key(module, entry),
            klib.read(entry).map_err(|error| error.to_string())?,
        );
    }
    Ok(())
}

/// Library compilation consumes Kotlin/Java inputs. Keep MODULE declarations while omitting
/// runtime JS/TS resources; the full fixture remains the execution input and cache identity.
pub(super) fn module_blocks(
    source: &str,
    target: TestTarget,
) -> Result<Vec<krusty::conformance::ModuleBlock>, String> {
    let prepared = krusty::conformance::prepare_test_source(source, target);
    let mut library_source = String::new();
    let mut compiler_input = true;
    for line in prepared.lines() {
        if krusty::conformance::directive_payload(line, "MODULE").is_some() {
            compiler_input = true;
        } else if let Some(name) = krusty::conformance::directive_payload(line, "FILE") {
            let extension = Path::new(name.trim())
                .extension()
                .and_then(|extension| extension.to_str());
            compiler_input = matches!(extension, Some("kt" | "kts" | "java"));
        }
        if compiler_input {
            library_source.push_str(line);
            library_source.push('\n');
        }
    }
    krusty::conformance::split_modules(&library_source)
        .ok_or_else(|| "unsupported module graph".into())
}

/// Source languages are identified from original FILE declarations, before a `.kt` suffix is
/// removed. The execution plan and cache key still retain the complete fixture, including scripts.
fn reference_units(
    source: &str,
    units: &[ModuleUnit],
    target: TestTarget,
) -> Result<Vec<ModuleUnit>, String> {
    let prepared = krusty::conformance::prepare_test_source(source, target);
    let stem = units
        .first()
        .and_then(|unit| unit.files.first())
        .map(|(stem, _)| stem.as_str())
        .unwrap_or("Case");
    let kotlin = krusty::conformance::kotlin_source_blocks(&prepared, stem)?;
    let files = |module: Option<&str>| {
        kotlin
            .iter()
            .filter(|block| block.module.as_deref() == module)
            .map(|block| {
                (
                    block.name.clone(),
                    super::super::box_reference_classes::strip_diagnostic_markers(&block.source),
                )
            })
            .collect::<Vec<_>>()
    };
    if krusty::conformance::contains_directive(source, "MODULE") {
        let mut blocks = module_blocks(source, target)?;
        for block in &mut blocks {
            block.files = files(Some(&block.name));
        }
        if krusty::conformance::directive(source, "WITH_COROUTINES") {
            krusty::conformance::inject_coroutine_support(
                &mut blocks,
                source,
                krusty::conformance::COROUTINE_HELPERS,
            )?;
        }
        Ok(krusty::conformance::module_units(&blocks))
    } else {
        let mut unit = units.first().ok_or("empty compilation plan")?.clone();
        unit.files = files(None);
        if krusty::conformance::directive(source, "WITH_COROUTINES") {
            unit.files.extend(krusty::conformance::coroutine_sources(
                source,
                krusty::conformance::COROUTINE_HELPERS,
            )?);
        }
        Ok(vec![unit])
    }
}

fn command(
    target: TestTarget,
    toolchain: &Toolchain,
    work: &Path,
    module: &str,
) -> Result<Command, String> {
    let mut command = Command::new(&toolchain.compiler);
    command.arg(format!("-Xklib-relative-path-base={}", work.display()));
    command.arg("-Xallow-kotlin-package");
    if target == TestTarget::Native {
        // ExtTestCaseGroupProvider supplies these opt-ins to every Native compilation.
        command.args([
            "-opt-in=kotlin.native.internal.InternalForKotlinNative",
            "-opt-in=kotlin.native.internal.InternalForKotlinNativeTests",
        ]);
        command.arg("-target").arg(
            super::native_target()?
                .konan_name()
                .ok_or("unsupported Konan target")?,
        );
        command.args([
            "-produce",
            "library",
            "-nopack",
            "-module-name",
            module,
            "-nostdlib",
        ]);
        command.arg("-output").arg(work.join("output"));
    } else {
        let wasm = match target {
            TestTarget::WasmJs => "wasm-js",
            TestTarget::WasmWasi => "wasm-wasi",
            _ => unreachable!(),
        };
        command.arg(format!("-Xwasm-target={wasm}")).args([
            "-Xir-produce-klib-dir",
            "-ir-output-name",
            module,
        ]);
        command.arg("-ir-output-dir").arg(work.join("output"));
    }
    Ok(command)
}

fn fragment_arguments(
    command: &mut Command,
    unit: &ModuleUnit,
    paths: &[PathBuf],
    built: &BTreeMap<String, BuiltModule>,
    stdlib: &Path,
    directive_libraries: &[PathBuf],
    graph: &FragmentGraph,
) -> Result<(), String> {
    if unit.fragments.len() <= 1 {
        return Ok(());
    }
    command.arg("-Xmulti-platform");
    // Wasm consumes folded platform representatives, as the JVM oracle does. Its separate KMP
    // scheme instead requires independently built common-metadata KLIB dependencies.
    let separate = graph.target == TestTarget::Native;
    if separate {
        command.arg("-Xseparate-kmp-compilation");
    }
    let mut offset = 0;
    for (index, fragment) in unit.fragments.iter().enumerate() {
        command.arg(format!("-Xfragments={}", fragment.name));
        for path in &paths[offset..offset + fragment.kotlin_file_count] {
            command.arg(format!(
                "-Xfragment-sources={}:{}",
                fragment.name,
                path.display()
            ));
        }
        offset += fragment.kotlin_file_count;
        for refined in &fragment.refines {
            command.arg(format!("-Xfragment-refines={}:{refined}", fragment.name));
        }
        if separate && index + 1 != unit.fragments.len() {
            // The selected stdlib is the fragment's platform declaration evidence too.
            command.arg(format!(
                "-Xfragment-dependency={}:{}",
                fragment.name,
                stdlib.display()
            ));
            for library in directive_libraries {
                command.arg(format!(
                    "-Xfragment-dependency={}:{}",
                    fragment.name,
                    library.display()
                ));
            }
            let block = graph
                .blocks
                .get(&fragment.name)
                .ok_or_else(|| format!("missing fragment {}", fragment.name))?;
            for (dependencies, flag) in [
                (&block.deps, "-Xfragment-dependency"),
                (&block.friends, "-Xfragment-friend-dependency"),
            ] {
                for dependency in dependencies {
                    let representative = graph
                        .representatives
                        .get(dependency)
                        .ok_or_else(|| format!("missing built representative for {dependency}"))?;
                    if representative == &unit.name {
                        continue;
                    }
                    let library = built
                        .get(representative)
                        .ok_or_else(|| format!("missing fragment dependency {representative}"))?;
                    command.arg(format!(
                        "{flag}={}:{}",
                        fragment.name,
                        library.path.display()
                    ));
                }
            }
        }
    }
    if offset != paths.len() {
        return Err(format!(
            "module {} fragment source count mismatch",
            unit.name
        ));
    }
    Ok(())
}

struct FragmentGraph {
    target: TestTarget,
    blocks: BTreeMap<String, krusty::conformance::ModuleBlock>,
    representatives: BTreeMap<String, String>,
}
impl FragmentGraph {
    fn new(source: &str, target: TestTarget, units: &[ModuleUnit]) -> Result<Self, String> {
        let mut blocks = if krusty::conformance::contains_directive(source, "MODULE") {
            module_blocks(source, target)?
        } else {
            Vec::new()
        };
        if !blocks.is_empty() && krusty::conformance::directive(source, "WITH_COROUTINES") {
            krusty::conformance::inject_coroutine_support(
                &mut blocks,
                source,
                krusty::conformance::COROUTINE_HELPERS,
            )?;
        }
        let mut representatives = BTreeMap::new();
        for unit in units {
            for fragment in &unit.fragments {
                representatives
                    .entry(fragment.name.clone())
                    .or_insert_with(|| unit.name.clone());
            }
        }
        Ok(Self {
            target,
            blocks: blocks
                .into_iter()
                .map(|block| (block.name.clone(), block))
                .collect(),
            representatives,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_atomicfu_case_compiles_once_its_directive_libraries_are_on_the_classpath() {
        let target = match std::env::var("KRUSTY_KLIB_BYTE_TEST_TARGET").as_deref() {
            Ok("native") => TestTarget::Native,
            Ok("wasm-wasi") => TestTarget::WasmWasi,
            Ok("wasm-js") | Err(_) => TestTarget::WasmJs,
            other => panic!("unknown KLIB byte test target {other:?}"),
        };
        let source = include_str!("../box_regressions/atomicfu/fieldUpdaters.kt");
        let units = super::super::units(source, "fieldUpdaters", target).unwrap();
        compile(target, source, &units)
            .expect("the atomicfu reference compile receives the directive plugin and runtime");
    }

    #[test]
    fn reference_links_a_platform_library_from_common_source_dependencies() {
        let target = match std::env::var("KRUSTY_KLIB_BYTE_TEST_TARGET").as_deref() {
            Ok("native") => TestTarget::Native,
            Ok("wasm-wasi") => TestTarget::WasmWasi,
            Ok("wasm-js") | Err(_) => TestTarget::WasmJs,
            other => panic!("unknown KLIB byte test target {other:?}"),
        };
        let source = "// LANGUAGE: +MultiPlatformProjects\n// MODULE: lib-common\nexpect class A<T> { fun foo(x: T): T }\n// MODULE: lib-platform()()(lib-common)\nactual class A<T> { actual fun foo(x: T): T = x }\n// MODULE: app-common(lib-common)\nfun useA(a: A<String>) = a.foo(\"OK\")\n// MODULE: app-platform(lib-platform)()(app-common)\nfun box() = useA(A<String>())\n";
        super::super::super::box_reference_classes::checked_box(source, "Case")
            .expect("whole-program JVM oracle");
        let units = super::super::units(source, "Case", target).unwrap();
        let (entries, _) = compile(target, source, &units).unwrap();
        assert!(entries.contains_key(&("app-platform".into(), "default/manifest".into())));
    }

    #[test]
    fn reference_sources_keep_kotlin_names_and_fragment_counts_without_compiling_resources() {
        let source = "// FILE: real.mjs.kt\nfun box() = <!DEBUG_INFO_EXPRESSION_TYPE(\"String\")!>\"OK\"<!>\n// FILE: helper.mjs\nexport const x = 1;\n";
        let actual = super::super::units(source, "Case", TestTarget::WasmJs).unwrap();
        let reference = reference_units(source, &actual, TestTarget::WasmJs).unwrap();
        assert_eq!(
            reference[0].files,
            [("real.mjs".into(), "fun box() = \"OK\"\n".into())]
        );
        assert_eq!(actual[0].files.len(), 2, "execution inventory is retained");
        let source = "// MODULE: common\n// FILE: real.mjs.kt\nexpect fun answer(): String\n// MODULE: main()()(common)\n// FILE: Main.kt\nactual fun answer() = <!DEBUG_INFO_EXPRESSION_TYPE(\"String\")!>\"OK\"<!>\nfun box() = answer()\n";
        let actual = super::super::units(source, "Case", TestTarget::WasmJs).unwrap();
        let reference = reference_units(source, &actual, TestTarget::WasmJs).unwrap();
        assert_eq!(reference.len(), 1);
        assert_eq!(
            reference[0].files,
            [
                ("real.mjs".into(), "expect fun answer(): String\n".into()),
                (
                    "Main".into(),
                    "actual fun answer() = \"OK\"\nfun box() = answer()\n".into()
                ),
            ]
        );
        assert_eq!(reference[0].common_file_count, 1);
        assert_eq!(reference[0].fragments[0].kotlin_file_count, 1);
        assert_eq!(reference[0].fragments[1].kotlin_file_count, 1);
        assert_eq!(actual[0].files.len(), 2, "execution inventory is retained");
    }

    #[test]
    fn cached_artifact_names_preserve_complete_module_identities() {
        for (module, entry) in [
            ("a/b", "default/manifest"),
            ("a", "b/default/manifest"),
            ("µ-module", "default/ir/files.knf"),
        ] {
            assert_eq!(
                decode_artifact_key(&artifact_key(module, entry)),
                Ok((module.to_owned(), entry.to_owned()))
            );
        }
        assert_ne!(
            artifact_key("a/b", "default/manifest"),
            artifact_key("a", "b/default/manifest")
        );
    }

    #[test]
    fn coroutine_support_remains_a_declared_dependency_of_common_fragments() {
        let source = format!(
            "// WITH_COROUTINES\n{}",
            include_str!("../box_regressions/multimodule/multimoduleDependsOnFriendDependency.kt")
        );
        let units = super::super::units(&source, "Case", TestTarget::Native).unwrap();
        let graph = FragmentGraph::new(&source, TestTarget::Native, &units).unwrap();
        assert_eq!(graph.blocks["common"].deps, ["friend", "support"]);
        assert_eq!(graph.blocks["common"].friends, ["friend"]);
        assert_eq!(graph.representatives["support"], "support");
        assert_eq!(graph.representatives["common"], "main");
    }
}
