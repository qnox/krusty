//! Indexed frontend analysis helpers for checker-focused integration tests.

use super::*;

pub(super) fn checker_diagnostics_with_classpath(
    main: &str,
    classpath: Vec<PathBuf>,
) -> Vec<String> {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let classpath = std::rc::Rc::new(Classpath::new(classpath));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let _ = krusty::frontend::analyze_source(main, platform, &mut diagnostics);
    diagnostics
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.clone())
        .collect()
}

pub fn inspect_checker_with_classpath<T>(
    main: &str,
    classpath: Vec<PathBuf>,
    inspect: impl FnOnce(
        &krusty::ast::File,
        &krusty::frontend::FrontendTypeInfo,
        &krusty::frontend::FrontendSymbols,
    ) -> T,
) -> (Vec<String>, T) {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let classpath = std::rc::Rc::new(Classpath::new(classpath));
    let platform = Box::new(
        krusty::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    );
    let (file, symbols, info) = krusty::frontend::analyze_source(main, platform, &mut diagnostics);
    let symbols = symbols.expect("inspection source must parse");
    let info = info.expect("inspection source signatures must finalize");
    let inspected = inspect(&file, &info, &symbols);
    (
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.clone())
            .collect(),
        inspected,
    )
}
