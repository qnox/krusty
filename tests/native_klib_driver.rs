//! Owned Native driver compilation: one selected KLIB snapshot supplies both semantic bodies and
//! import-aware linkage. Checked entry ids and backend-produced headers are the only C entry seam.

use krusty::diag::{DiagSink, Severity};
use krusty::klib_libraries::{KlibLibraryInput, KlibStdlibSemantics};
use krusty_native::{CraneliftBackend, DriverEntry, Entry, ImportLibrary, NativeTarget};

pub struct Compilation {
    pub objects: Vec<Vec<u8>>,
    pub header: Vec<u8>,
    imports: Vec<ImportLibrary>,
    target: NativeTarget,
}

impl Compilation {
    pub fn image(&self, extra: &[&[u8]]) -> Vec<u8> {
        let objects = self
            .objects
            .iter()
            .map(Vec::as_slice)
            .chain(extra.iter().copied())
            .collect::<Vec<_>>();
        krusty_native::link_program_with_libraries(&objects, &self.imports, self.target)
            .expect("link compiler-generated Native fixture and C driver")
    }
}

pub fn compile(
    sources: &[(&str, &str)],
    target: NativeTarget,
    module: &str,
    entries: &[(u32, &str, &str)],
    entry: Option<Entry>,
) -> Compilation {
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(
        krusty::compilation_target::CompilationTarget::Native,
    )
    .expect("Native driver requires its selected Native stdlib KLIB");
    let (libraries, imports) = krusty_native::NativeKlibInputs::open(
        &[KlibLibraryInput::platform_stdlib(
            stdlib,
            KlibStdlibSemantics::Native,
        )],
        target,
    )
    .expect("open selected Native KLIB inputs")
    .into_parts();
    let inputs = sources
        .iter()
        .map(|(stem, source)| krusty::source::SourceInput::kotlin(source).with_file_stem(stem))
        .collect::<Vec<_>>();
    let stems = sources
        .iter()
        .map(|(stem, _)| stem.to_string())
        .collect::<Vec<_>>();
    let mut features = krusty::features::LangFeatures::from_environment();
    for (_, source) in sources {
        features.apply_source_directives(source);
    }
    let mut diags = DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_streaming_with_features(
        &inputs,
        krusty::frontend::PlatformProvider::new(
            krusty::compilation_target::CompilationTarget::Native,
            Box::new(libraries),
        ),
        &features,
        &mut diags,
    );
    assert!(!diags.has_errors(), "{module}: {:?}", diags.diags);
    let entries = entries
        .iter()
        .map(|(file, name, symbol)| DriverEntry {
            callable: analysis
                .fixture_function(*file, name)
                .expect("select checked fixture entry"),
            symbol: symbol.to_string(),
        })
        .collect();
    let backend = CraneliftBackend::new(target)
        .verified()
        .with_driver_entries(module, entries);
    let backend = match entry {
        Some(entry) => backend.with_entry(entry),
        // The sole module here is `module`; C supplies the program entry. Keep the backend's
        // ordinary per-file initialization trampolines for all selected fixture exports.
        None => backend.with_entry_module(format!("{module}.C-program-entry")),
    };
    let artifacts = krusty::compiler::emit_analyzed(analysis, &stems, &backend, module, &mut diags);
    assert_eq!(
        diags
            .diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.msg.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new(),
        "{module}"
    );
    let header = artifacts
        .iter()
        .find(|(name, _)| name == &format!("{module}.h"))
        .expect("backend-produced fixture header")
        .1
        .clone();
    let objects = artifacts
        .into_iter()
        .filter_map(|(name, bytes)| name.ends_with(".o").then_some(bytes))
        .collect::<Vec<_>>();
    assert!(!objects.is_empty(), "{module}: no Native objects");
    Compilation {
        objects,
        header,
        imports,
        target,
    }
}
