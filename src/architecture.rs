#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn frontend_facade_has_no_backend_edges() {
        assert_no_patterns(
            "src/frontend.rs",
            &[
                "crate::backend",
                "crate::compiler",
                "crate::ir",
                "crate::ir_lower",
                "crate::jvm",
                "crate::js",
            ],
        );
    }

    #[test]
    fn backend_contract_has_no_frontend_or_target_edges() {
        assert_no_patterns(
            "src/backend.rs",
            &[
                "check_file",
                "collect_signatures",
                "crate::compiler",
                "crate::frontend",
                "crate::ir_lower",
                "crate::jvm",
                "crate::js",
                "crate::lexer",
                "crate::parser",
            ],
        );
    }

    #[test]
    fn compiler_driver_has_no_concrete_target_edges() {
        assert_no_patterns(
            "src/compiler.rs",
            &["crate::ir_lower", "crate::jvm", "crate::js"],
        );
    }

    #[test]
    fn lsp_facade_has_no_backend_or_target_edges() {
        assert_no_patterns(
            "src/lsp.rs",
            &[
                "crate::backend",
                "crate::compiler",
                "crate::ir",
                "crate::ir_lower",
                "crate::jvm",
                "crate::js",
            ],
        );
    }

    #[test]
    fn target_modules_do_not_depend_on_compiler_driver() {
        for path in rust_files_under("src/jvm")
            .into_iter()
            .chain(rust_files_under("src/js"))
        {
            assert_file_has_no_patterns(&path, &["crate::compiler"]);
        }
    }

    #[test]
    fn ir_lower_has_only_allowlisted_target_edges() {
        assert_only_allowlisted_patterns(
            "src/ir_lower.rs",
            "crate::jvm",
            &[
                "use crate::jvm::names::{property_getter_name, property_setter_name};",
                "crate::jvm::suspend::zero_value",
            ],
        );
    }

    fn assert_no_patterns(relative: &str, patterns: &[&str]) {
        assert_file_has_no_patterns(&source_path(relative), patterns);
    }

    fn assert_file_has_no_patterns(path: &Path, patterns: &[&str]) {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let offenders: Vec<_> = patterns
            .iter()
            .copied()
            .filter(|pattern| text.contains(pattern))
            .collect();
        assert!(
            offenders.is_empty(),
            "{} contains forbidden dependency markers: {}",
            path.display(),
            offenders.join(", ")
        );
    }

    fn assert_only_allowlisted_patterns(relative: &str, pattern: &str, allowlist: &[&str]) {
        let path = source_path(relative);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let offenders: Vec<_> = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(pattern))
            .filter(|(_, line)| !allowlist.iter().any(|allowed| line.contains(allowed)))
            .map(|(idx, line)| format!("{}:{}", idx + 1, line.trim()))
            .collect();
        assert!(
            offenders.is_empty(),
            "{} contains unallowlisted `{}` references:\n{}",
            path.display(),
            pattern,
            offenders.join("\n")
        );
    }

    fn rust_files_under(relative: &str) -> Vec<PathBuf> {
        let root = source_path(relative);
        let mut files = Vec::new();
        collect_rust_files(&root, &mut files);
        files
    }

    fn collect_rust_files(path: &Path, files: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|err| panic!("failed to read directory entry: {err}"))
                .path();
            if path.is_dir() {
                collect_rust_files(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    fn source_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
    }
}
