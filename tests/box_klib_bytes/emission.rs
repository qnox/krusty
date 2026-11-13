//! Emit each source module as a KLIB against its declared generated dependencies.
use krusty::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use krusty::conformance::{ModuleUnit, TestTarget};
use krusty::diag::DiagSink;
use krusty::klib::backend::KlibBackend;
use krusty::klib::write::{KlibPlatform, KlibStamp};

use super::{dependencies, BuiltModule, QualifiedArtifacts, Scratch};

pub(super) fn compile(
    target: TestTarget,
    source: &str,
    units: &[ModuleUnit],
) -> Result<QualifiedArtifacts, String> {
    let features = krusty::features::LangFeatures::from_source(source);
    let backend = backend(target, source)?;
    let scratch = Scratch::new()?;
    let mut built = BTreeMap::new();
    let mut inventory = QualifiedArtifacts::new();
    for (ordinal, unit) in units.iter().enumerate() {
        let (libraries, friends) = dependencies(unit, &built)?;
        let mut diags = DiagSink::new();
        let declarations = libraries_for(target, &libraries, &friends)?;
        let mut module_features = features.clone();
        if unit.common_file_count != 0 {
            module_features.enable("MultiPlatformProjects");
        }
        let inputs = unit
            .files
            .iter()
            .enumerate()
            .map(|(index, (stem, text))| {
                let input = krusty::source::SourceInput::kotlin(text).with_file_stem(stem);
                if index < unit.common_file_count {
                    input.common()
                } else {
                    input
                }
            })
            .collect::<Vec<_>>();
        let stems = unit
            .files
            .iter()
            .map(|(stem, _)| stem.clone())
            .collect::<Vec<_>>();
        let analysis = krusty::frontend::analyze_source_set_streaming_with_module(
            &inputs,
            krusty::frontend::PlatformProvider::new(
                target.compilation_target(),
                Box::new(declarations),
            ),
            &module_features,
            &unit.name,
            &mut diags,
        );
        let artifacts =
            krusty::compiler::emit_analyzed(analysis, &stems, &backend, &unit.name, &mut diags);
        if diags.has_errors() {
            return Err(diags
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>()
                .join("\n"));
        }
        if artifacts.is_empty() {
            return Err(format!("module {} emitted no KLIB entries", unit.name));
        }
        let root = scratch.0.join(format!("module-{ordinal}"));
        for (name, bytes) in artifacts {
            let path = Path::new(&name);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(format!("unsafe KLIB entry {name:?}"));
            }
            let output = root.join(&name);
            std::fs::create_dir_all(output.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(output, &bytes).map_err(|e| e.to_string())?;
            if inventory.insert((unit.name.clone(), name), bytes).is_some() {
                return Err("duplicate KLIB entry".into());
            }
        }
        built.insert(
            unit.name.clone(),
            BuiltModule {
                path: root,
                dependencies: libraries,
            },
        );
    }
    Ok(inventory)
}

thread_local! {
    static STDLIBS: std::cell::RefCell<Vec<(TestTarget, KlibLibraries)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn libraries_for(
    target: TestTarget,
    dependencies: &[PathBuf],
    friends: &[PathBuf],
) -> Result<KlibLibraries, String> {
    if dependencies.is_empty() {
        return STDLIBS.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((_, libraries)) = cache.iter().find(|(cached, _)| *cached == target) {
                return Ok(libraries.for_compilation());
            }
            let libraries = open_libraries(target, dependencies, friends)?;
            let compilation = libraries.for_compilation();
            cache.push((target, libraries));
            Ok(compilation)
        });
    }
    open_libraries(target, dependencies, friends)
}

fn open_libraries(
    target: TestTarget,
    dependencies: &[PathBuf],
    friends: &[PathBuf],
) -> Result<KlibLibraries, String> {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(target.compilation_target())
        .ok_or("no exact target stdlib")?;
    let semantics = match target {
        TestTarget::Native => KlibStdlibSemantics::Native,
        TestTarget::WasmJs | TestTarget::WasmWasi => KlibStdlibSemantics::Wasm,
        TestTarget::Jvm => return Err("JVM declarations are not KLIBs".into()),
    };
    let mut inputs = dependencies
        .iter()
        .map(|path| {
            if friends.contains(path) {
                KlibLibraryInput::friend(path.clone())
            } else {
                KlibLibraryInput::dependency(path.clone())
            }
        })
        .collect::<Vec<_>>();
    inputs.push(KlibLibraryInput::platform_stdlib(stdlib, semantics));
    inputs.extend(super::super::box_lane::wasm_test_library(
        target.compilation_target(),
    ));
    KlibLibraries::open(&inputs).map_err(|error| error.to_string())
}

pub(crate) fn backend(target: TestTarget, source: &str) -> Result<KlibBackend, String> {
    let features = krusty::features::LangFeatures::from_source(source);
    let platform = match target {
        TestTarget::Native => KlibPlatform::Native,
        TestTarget::WasmJs => KlibPlatform::WasmJs,
        TestTarget::WasmWasi => KlibPlatform::WasmWasi,
        TestTarget::Jvm => return Err("JVM artifacts are class files".into()),
    };
    let mut backend = KlibBackend::new(
        platform,
        KlibStamp::new(
            krusty::kotlin_version::target(),
            features.language_version(),
        ),
    )
    .with_annotations_in_metadata(features.has("AnnotationsInMetadata"));
    if target == TestTarget::Native {
        backend = backend
            .with_native_target(super::native_target()?)
            .map_err(|error| error.to_string())?;
    }
    Ok(backend)
}
