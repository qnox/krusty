//! Temporary: in-process mirror of the LSP analysis pipeline for one JPS project file.
//! Usage: repro_dump <project-root> <target.kt>

use std::path::PathBuf;
use std::rc::Rc;

use krusty::features::LangFeatures;
use krusty::frontend;
use krusty::source::SourceInput;
use krusty_lsp::project::provider::ProjectProvider;
use krusty_lsp::project::runner::ProcessRunner;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: repro_dump <root> <target.kt>"));
    let target = PathBuf::from(args.next().expect("usage: repro_dump <root> <target.kt>"));

    let provider = krusty_lsp::project::jps::JpsProvider::new(root);
    let runner = ProcessRunner;
    let model = provider.probe(&runner).expect("jps probe");
    eprintln!("model: {} modules", model.modules.len());

    let mut classpath_entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for module in &model.modules {
        for entry in model.compile_classpath(module) {
            if seen.insert(entry.clone()) {
                classpath_entries.push(entry);
            }
        }
    }
    classpath_entries.extend(krusty::toolchain::jdk_modules());
    eprintln!("classpath: {} entries", classpath_entries.len());

    let graph = model.into_source_module_graph();
    let text = std::fs::read_to_string(&target).expect("read target");
    let uri = url::Url::from_file_path(&target).expect("uri").to_string();

    let mut sources = krusty_lsp::ProjectSources::default();
    let (support, inferred_support, java_sources) = sources
        .load(
            &graph,
            &[(&uri, &text)],
            &[&uri],
            krusty_lsp::MAX_SOURCE_SET_BYTES,
        )
        .expect("load project sources");
    eprintln!(
        "support: {} files ({} inferred), {} java",
        support.len(),
        inferred_support,
        java_sources.len()
    );

    let classpath = Rc::new(krusty::jvm::classpath::Classpath::new(classpath_entries));
    classpath.prepare_for_source_analysis();
    if !java_sources.is_empty() {
        let java: Vec<(String, String)> = java_sources
            .iter()
            .map(|source| (String::new(), source.clone()))
            .collect();
        let resolve = |cand: &str| {
            classpath
                .find_name(krusty::types::type_name(cand))
                .is_some()
        };
        if let Some(stubs) = krusty::jvm::java_stub::stub_classes(
            &java,
            krusty::jvm::java_stub::StubMode::Lenient,
            &resolve,
        ) {
            classpath.set_stub_overlay(stubs);
        } else {
            eprintln!("WARN: java stubbing failed");
        }
    }

    let mut inputs = vec![SourceInput::kotlin(&text)];
    inputs.extend(
        support
            .iter()
            .map(|(_, source)| SourceInput::kotlin(source)),
    );
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(classpath));
    let mut diags = krusty::diag::DiagSink::new();
    let _ = frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1 + inferred_support,
        platform,
        &LangFeatures::new(),
        &mut diags,
    );
    let mine: Vec<_> = diags.diags.iter().filter(|d| d.file == 0).collect();
    println!("target diagnostics: {}", mine.len());
    for d in &mine {
        println!("  {}..{} {}", d.span.lo, d.span.hi, d.msg);
    }
}
