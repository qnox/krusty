#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn frontend_facade_uses_only_frontend_dependencies() {
        assert_allowed_crate_modules(
            "src/frontend.rs",
            &[
                "ast",
                "diag",
                "features",
                "lexer",
                "libraries",
                "parser",
                "resolve",
            ],
        );
    }

    #[test]
    fn parser_uses_only_syntax_layer_dependencies() {
        assert_allowed_crate_modules(
            "src/parser.rs",
            &["ast", "diag", "features", "token", "types"],
        );
    }

    #[test]
    fn backend_contract_uses_only_frontend_handoff_dependencies() {
        assert_allowed_crate_modules("src/backend.rs", &["diag", "frontend"]);
    }

    #[test]
    fn compiler_driver_uses_only_frontend_and_backend_contracts() {
        assert_allowed_crate_modules("src/compiler.rs", &["ast", "backend", "diag", "frontend"]);
    }

    #[test]
    fn lsp_facade_uses_only_frontend_analysis_dependencies() {
        assert_allowed_crate_modules("src/lsp.rs", &["ast", "diag", "frontend", "libraries"]);
    }

    #[test]
    fn jvm_target_modules_use_only_jvm_side_dependencies() {
        let allowed = [
            "ast",
            "backend",
            "diag",
            "frontend",
            "ir",
            "ir_lower",
            "jvm",
            "libraries",
            "lru",
            "metadata",
            "module_symbols",
            "names",
            "plugins",
            "runtime",
            "symbol_resolver",
            "symbol_source",
            "toolchain",
            "trace",
            "trace_compiler",
            "types",
        ];
        for path in rust_files_under("src/jvm") {
            assert_allowed_crate_modules_in_file(&path, &allowed);
        }
    }

    #[test]
    fn jvm_backend_adapter_uses_only_frontend_handoff_and_jvm_dependencies() {
        assert_allowed_crate_modules(
            "src/jvm/backend.rs",
            &[
                "ast",
                "backend",
                "diag",
                "frontend",
                "ir",
                "ir_lower",
                "jvm",
                "metadata",
                "module_symbols",
                "plugins",
                "symbol_resolver",
                "trace_compiler",
                "types",
            ],
        );
    }

    #[test]
    fn js_facade_has_no_crate_dependencies() {
        assert_allowed_crate_modules("src/js/mod.rs", &[]);
    }

    #[test]
    fn js_emitter_uses_only_ir_contract_dependencies() {
        assert_allowed_crate_modules("src/js/emit.rs", &["ir", "types"]);
    }

    #[test]
    fn js_backend_adapter_uses_only_common_backend_dependencies() {
        assert_allowed_crate_modules(
            "src/js/backend.rs",
            &["backend", "diag", "frontend", "ir_lower", "runtime"],
        );
    }

    #[test]
    fn ir_lower_uses_only_common_lowering_dependencies() {
        assert_allowed_crate_modules(
            "src/ir_lower.rs",
            &[
                "ast",
                "frontend",
                "ir",
                "libraries",
                "module_symbols",
                "names",
                "runtime",
                "symbol_resolver",
                "synthetics",
                "trace_compiler",
                "types",
            ],
        );
    }

    #[test]
    fn module_symbols_uses_only_frontend_symbol_handoff_dependencies() {
        assert_allowed_crate_modules(
            "src/module_symbols.rs",
            &["frontend", "libraries", "symbol_source", "types"],
        );
    }

    #[test]
    fn synthetics_registry_uses_only_ir_contract_dependencies() {
        assert_allowed_crate_modules("src/synthetics.rs", &["ast", "ir", "types"]);
    }

    #[test]
    fn runtime_contract_uses_only_semantic_library_and_type_dependencies() {
        assert_allowed_crate_modules("src/runtime.rs", &["libraries", "types"]);
    }

    #[test]
    fn semantic_library_contract_uses_only_symbol_source_and_type_dependencies() {
        assert_allowed_crate_modules("src/libraries.rs", &["name_tree", "symbol_source", "types"]);
    }

    #[test]
    fn native_plugins_use_only_plugin_and_ir_contract_dependencies() {
        assert_allowed_crate_modules_in_tree(
            "src/plugins",
            &[
                "ast",
                "diag",
                "ir",
                "libraries",
                "lexer",
                "names",
                "parser",
                "plugins",
                "types",
            ],
        );
    }

    #[test]
    fn frontend_tools_use_only_their_declared_frontend_handoff_dependencies() {
        assert_allowed_crate_modules("src/bin/check.rs", &["diag", "frontend", "lexer", "parser"]);
        assert_allowed_crate_modules(
            "src/bin/blockers.rs",
            &["diag", "frontend", "lexer", "parser"],
        );
        assert_allowed_crate_modules(
            "src/bin/irbail.rs",
            &[
                "diag",
                "frontend",
                "ir_lower",
                "lexer",
                "libraries",
                "parser",
            ],
        );
        assert_allowed_crate_modules(
            "src/bin/bytediff.rs",
            &["diag", "frontend", "ir_lower", "jvm", "lexer", "parser"],
        );
        assert_allowed_crate_modules(
            "src/bin/survey.rs",
            &[
                "ast",
                "conformance",
                "diag",
                "features",
                "frontend",
                "ir",
                "ir_lower",
                "jvm",
                "lexer",
                "parser",
                "toolchain",
            ],
        );
    }

    #[test]
    fn integration_tests_use_only_public_compiler_layer_dependencies() {
        assert_allowed_external_crate_modules_in_tree(
            "tests",
            &[
                "ast",
                "compiler",
                "conformance",
                "dhat",
                "diag",
                "features",
                "frontend",
                "ir",
                "ir_lower",
                "js",
                "jvm",
                "lexer",
                "libraries",
                "metadata",
                "parser",
                "plugins",
                "symbol_resolver",
                "symbol_source",
                "toolchain",
                "types",
            ],
        );
    }

    #[test]
    fn dependency_collector_handles_rust_paths_and_ignores_test_modules() {
        let source = r#"
            use crate::{ast, jvm::names};
            use krusty;
            use krusty::diag;

            fn f() {
                let _ = crate :: js :: SOME;
                let _ = krusty::frontend::analyze_source_standalone;
            }

            #[cfg(test)]
            mod tests {
                use crate::frontend;
            }
        "#;

        assert_eq!(
            crate_modules(source),
            BTreeSet::from([
                "ast".to_string(),
                "diag".to_string(),
                "frontend".to_string(),
                "js".to_string(),
                "jvm".to_string(),
            ])
        );
    }

    #[test]
    fn external_dependency_collector_ignores_local_test_crate_paths() {
        let source = r#"
            use crate::common;
            use super::fixtures;
            use krusty::frontend;

            fn f() {
                let _ = crate::common::compile;
                let _ = krusty::jvm::names::file_class_name;
            }
        "#;

        assert_eq!(
            external_crate_modules(source),
            BTreeSet::from(["frontend".to_string(), "jvm".to_string()])
        );
    }

    fn assert_allowed_crate_modules(relative: &str, allowed: &[&str]) {
        assert_allowed_crate_modules_in_file(&source_path(relative), allowed);
    }

    fn assert_allowed_crate_modules_in_tree(relative: &str, allowed: &[&str]) {
        for path in rust_files_under(relative) {
            assert_allowed_crate_modules_in_file(&path, allowed);
        }
    }

    fn assert_allowed_external_crate_modules_in_tree(relative: &str, allowed: &[&str]) {
        for path in rust_files_under(relative) {
            assert_allowed_external_crate_modules_in_file(&path, allowed);
        }
    }

    fn assert_allowed_crate_modules_in_file(path: &Path, allowed: &[&str]) {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let allowed: BTreeSet<_> = allowed.iter().copied().collect();
        let actual = crate_modules(&text);
        let offenders: Vec<_> = actual
            .iter()
            .filter(|module| !allowed.contains(module.as_str()))
            .map(String::as_str)
            .collect();
        assert!(
            offenders.is_empty(),
            "{} uses crate modules outside its dependency budget: {}",
            path.display(),
            offenders.join(", ")
        );
    }

    fn assert_allowed_external_crate_modules_in_file(path: &Path, allowed: &[&str]) {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let allowed: BTreeSet<_> = allowed.iter().copied().collect();
        let actual = external_crate_modules(&text);
        let offenders: Vec<_> = actual
            .iter()
            .filter(|module| !allowed.contains(module.as_str()))
            .map(String::as_str)
            .collect();
        assert!(
            offenders.is_empty(),
            "{} uses external crate modules outside its dependency budget: {}",
            path.display(),
            offenders.join(", ")
        );
    }

    fn crate_modules(text: &str) -> BTreeSet<String> {
        modules_for_roots(text, &["crate", env!("CARGO_PKG_NAME")])
    }

    fn external_crate_modules(text: &str) -> BTreeSet<String> {
        modules_for_roots(text, &[env!("CARGO_PKG_NAME")])
    }

    fn modules_for_roots(text: &str, roots: &[&str]) -> BTreeSet<String> {
        let file =
            syn::parse_file(text).unwrap_or_else(|err| panic!("failed to parse Rust: {err}"));
        let mut modules = BTreeSet::new();
        let mut visitor = CrateDependencyVisitor {
            modules: &mut modules,
            roots,
        };
        syn::visit::visit_file(&mut visitor, &file);
        modules
    }

    struct CrateDependencyVisitor<'a> {
        modules: &'a mut BTreeSet<String>,
        roots: &'a [&'a str],
    }

    impl<'ast> syn::visit::Visit<'ast> for CrateDependencyVisitor<'_> {
        fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
            if has_cfg_test(&item.attrs) {
                return;
            }
            syn::visit::visit_item_mod(self, item);
        }

        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            collect_use_tree(&item.tree, &mut Vec::new(), self.modules, self.roots);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            collect_path_module(path, self.modules, self.roots);
            syn::visit::visit_path(self, path);
        }
    }

    fn collect_path_module(path: &syn::Path, modules: &mut BTreeSet<String>, roots: &[&str]) {
        let mut segments = path.segments.iter();
        if segments
            .next()
            .is_some_and(|segment| is_crate_root(segment, roots))
        {
            if let Some(module) = segments.next() {
                modules.insert(module.ident.to_string());
            }
        }
    }

    fn collect_use_tree(
        tree: &syn::UseTree,
        prefix: &mut Vec<String>,
        modules: &mut BTreeSet<String>,
        roots: &[&str],
    ) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                collect_prefixed_module(prefix, modules, roots);
                collect_use_tree(&path.tree, prefix, modules, roots);
                prefix.pop();
            }
            syn::UseTree::Name(name) => collect_terminal_use(&name.ident, prefix, modules, roots),
            syn::UseTree::Rename(rename) => {
                collect_terminal_use(&rename.ident, prefix, modules, roots)
            }
            syn::UseTree::Glob(_) => collect_prefixed_module(prefix, modules, roots),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    collect_use_tree(item, prefix, modules, roots);
                }
            }
        }
    }

    fn collect_terminal_use(
        ident: &syn::Ident,
        prefix: &[String],
        modules: &mut BTreeSet<String>,
        roots: &[&str],
    ) {
        if prefix
            .first()
            .is_some_and(|segment| is_crate_root_name(segment, roots))
        {
            if let Some(module) = prefix.get(1) {
                modules.insert(module.clone());
            } else if !is_crate_root_name(&ident.to_string(), roots) {
                modules.insert(ident.to_string());
            }
        }
    }

    fn collect_prefixed_module(prefix: &[String], modules: &mut BTreeSet<String>, roots: &[&str]) {
        if prefix
            .first()
            .is_some_and(|segment| is_crate_root_name(segment, roots))
        {
            if let Some(module) = prefix.get(1) {
                modules.insert(module.clone());
            }
        }
    }

    fn is_crate_root(segment: &syn::PathSegment, roots: &[&str]) -> bool {
        is_crate_root_name(&segment.ident.to_string(), roots)
    }

    fn is_crate_root_name(segment: &str, roots: &[&str]) -> bool {
        roots.contains(&segment)
    }

    fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path().is_ident("cfg") && {
                let mut found = false;
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("test") {
                        found = true;
                    }
                    Ok(())
                });
                found
            }
        })
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
