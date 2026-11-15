//! Coroutine files supplied by the pinned Kotlin box runner's source provider.
use super::{directive, inject_support_source_files, ModuleBlock, SourceBlock};

fn additional_helpers(source: &str) -> Vec<&'static str> {
    let mut files = Vec::new();
    if directive(source, "CHECK_STATE_MACHINE") {
        files.extend(["CoroutineUtil", "StateMachineChecker"]);
    }
    if directive(source, "CHECK_TAIL_CALL_OPTIMIZATION") {
        files.push("TailCallOptimizationChecker");
    }
    files
}

/// Preserve the caller's basic helper source. Extended runner modes use the original files,
/// retaining their names and declarations from this exact version's corpus checkout.
pub fn coroutine_sources(source: &str, basic_helpers: &str) -> Result<Vec<SourceBlock>, String> {
    if !directive(source, "WITH_COROUTINES") {
        return Ok(Vec::new());
    }
    let additional = additional_helpers(source);
    if additional.is_empty() {
        return Ok(vec![("CoroutineUtil".into(), basic_helpers.into())]);
    }
    let corpus = crate::toolchain::box_corpus_dir()
        .ok_or("coroutine helper sources require the pinned corpus")?;
    let root = corpus
        .ancestors()
        .nth(4)
        .ok_or("missing Kotlin corpus checkout root")?;
    let directory = root.join("compiler/testData/diagnostics/helpers/coroutines");
    std::iter::once("CoroutineHelpers")
        .chain(additional)
        .map(|name| {
            let path = directory.join(format!("{name}.kt"));
            std::fs::read_to_string(&path)
                .map(|source| (name.into(), source))
                .map_err(|error| {
                    format!("read pinned coroutine helper {}: {error}", path.display())
                })
        })
        .collect()
}

/// The same implicit support module for executable and reference module graphs.
pub fn inject_coroutine_support(
    modules: &mut Vec<ModuleBlock>,
    source: &str,
    basic_helpers: &str,
) -> Result<(), String> {
    if modules.iter().any(|module| module.name == "support") {
        return Ok(());
    }
    let sources = coroutine_sources(source, basic_helpers)?;
    if !sources.is_empty() {
        inject_support_source_files(modules, sources);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_modes_select_the_runner_files_in_provider_order() {
        assert_eq!(
            additional_helpers("// CHECK_STATE_MACHINE"),
            ["CoroutineUtil", "StateMachineChecker"]
        );
        assert_eq!(
            additional_helpers("// CHECK_TAIL_CALL_OPTIMIZATION"),
            ["TailCallOptimizationChecker"]
        );
        assert_eq!(
            additional_helpers("// CHECK_STATE_MACHINE\n// CHECK_TAIL_CALL_OPTIMIZATION"),
            [
                "CoroutineUtil",
                "StateMachineChecker",
                "TailCallOptimizationChecker"
            ]
        );
        assert_eq!(
            coroutine_sources("// CHECK_STATE_MACHINE", "custom").unwrap(),
            Vec::<SourceBlock>::new()
        );
        assert_eq!(
            coroutine_sources("// WITH_COROUTINES", "custom").unwrap(),
            [("CoroutineUtil".into(), "custom".into())]
        );
    }
}
