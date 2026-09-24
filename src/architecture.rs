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
                "fir",
                "lexer",
                "libraries",
                "parser",
                // The native compiler plugins a compilation selected are an analysis input: signature
                // collection and body checking host them, so the facade takes the selection.
                "plugins",
                "resolve",
                "source",
                "trace_compiler",
            ],
        );
    }

    #[test]
    fn parser_uses_only_syntax_layer_dependencies() {
        // `wide_stack` is cross-layer stack-growth infrastructure, not a semantic dependency: the
        // parser's expression recursion is depth-bounded like the checker's and lowering's, and
        // grows the same-thread stack per level so the bound survives any thread's stack size.
        assert_allowed_crate_modules(
            "src/parser.rs",
            &[
                "ast",
                "diag",
                "features",
                "kt_string",
                "token",
                "types",
                "wide_stack",
            ],
        );
    }

    #[test]
    fn dump_printers_depend_only_on_their_data_contracts() {
        assert_allowed_crate_modules("src/ast_print.rs", &["ast", "diag"]);
        assert_allowed_crate_modules("src/ir_print.rs", &["ir"]);
    }

    #[test]
    fn dump_assembly_only_combines_existing_data_contracts() {
        assert_allowed_crate_modules(
            "src/dump.rs",
            &[
                "ast",
                "ast_print",
                "diag",
                "frontend",
                "ir",
                "ir_print",
                "name_tree",
            ],
        );
    }

    #[test]
    fn metadata_wire_decoders_are_target_neutral() {
        assert_allowed_crate_modules_in_tree("src/metadata/decode", &[]);
    }

    #[test]
    fn backend_contract_uses_only_frontend_handoff_dependencies() {
        // The streaming handoff owns one common-IR unit plus a classifier-only semantic view.
        // `CheckedIrFile` cannot expose parser arenas, source maps, resolver entry points, or the
        // frontend symbol table.
        // It also names the native plugins the frontend ran (a selection, not a plugin's state), so
        // the backend runs exactly those.
        assert_allowed_crate_modules("src/backend.rs", &["diag", "fir", "ir", "plugins"]);
        assert_allowed_crate_modules_in_tree(
            "src/backend",
            &[
                "diag",
                "fir",
                "frontend",
                "ir",
                "libraries",
                "symbol_source",
                "types",
            ],
        );
    }

    #[test]
    fn compiler_driver_uses_only_frontend_and_backend_contracts() {
        assert_allowed_crate_modules(
            "src/compiler.rs",
            &[
                "ast",
                "backend",
                "diag",
                "fir",
                "fir_lower",
                "frontend",
                "ir",
                "resolve",
                "trace_compiler",
                "types",
            ],
        );
    }

    #[test]
    fn lsp_compiler_analysis_uses_only_frontend_dependencies() {
        // `plugins` is in budget for the analysis entry point alone: an editor reads no plugin
        // switches, so it selects every native extension from the registry explicitly.
        assert_allowed_external_crate_modules_in_tree(
            "crates/krusty-lsp/src/compiler_analysis",
            &[
                "ast",
                "diag",
                "frontend",
                "java_source",
                "libraries",
                "source",
                "symbol_source",
                "types",
            ],
        );
        assert_allowed_external_crate_modules_in_file(
            Path::new("crates/krusty-lsp/src/compiler_analysis.rs"),
            &[
                "ast",
                "diag",
                "features",
                "fir",
                "frontend",
                "libraries",
                "plugins",
                "source",
                "symbol_source",
                "types",
            ],
        );
    }

    #[test]
    fn lsp_dependency_sources_use_only_materialization_dependencies() {
        assert_allowed_external_crate_modules_in_tree(
            "crates/krusty-lsp/src/dependency_sources",
            &["diag", "jvm", "libraries", "symbol_source", "types"],
        );
    }

    #[test]
    fn compiler_library_has_no_command_line_layer() {
        assert!(
            !Path::new("src/cli.rs").exists(),
            "batch command-line parsing belongs to the krusty-cli package"
        );
        let library = fs::read_to_string("src/lib.rs").expect("read compiler library root");
        assert!(
            !library.lines().any(|line| line.trim() == "pub mod cli;"),
            "the compiler library must not export batch CLI policy"
        );
    }

    #[test]
    fn compiler_cli_uses_only_public_compiler_layers() {
        // `plugins` is the compiler-plugin surface, in budget as `features` is: the CLI reads
        // kotlinc's `-Xplugin`/`-P` switches and resolves them against the extension registry.
        assert_allowed_external_crate_modules_in_tree(
            "crates/krusty-cli/src",
            &[
                "compiler", "diag", "features", "frontend", "jvm", "plugins", "source",
            ],
        );
    }

    #[test]
    fn lsp_server_crate_uses_only_public_compiler_layers() {
        for path in rust_files_under("crates/krusty-lsp/src") {
            if path.ends_with("compiler_analysis.rs")
                || path
                    .components()
                    .any(|component| component.as_os_str() == "compiler_analysis")
                || path
                    .components()
                    .any(|component| component.as_os_str() == "dependency_sources")
            {
                continue;
            }
            let mut allowed = vec!["analysis", "diag", "jvm", "source", "types"];
            // `features` is the compiler's language-toggle surface. It is in budget exactly where a
            // module turns a PROJECT's own configuration into compiler settings: option parsing, the
            // project sync that reads them off the model, the analysis worker that applies them, and
            // the parity scanner, which is a batch worker applying each module's own toggles.
            if path.ends_with("options.rs")
                || path.ends_with("project/sync.rs")
                || path.ends_with("worker.rs")
                || path.ends_with("parity.rs")
                || path.ends_with("jvm_analysis.rs")
            {
                allowed.push("features");
            }
            // Standalone analysis has no project model to supply a configured target. Keep JDK
            // discovery in one explicit JVM adapter; compiler_analysis itself remains target-free.
            if path.ends_with("jvm_analysis.rs") {
                allowed.push("toolchain");
            }
            // The worker renders the dev-mode dump. Only the presentation layer is in budget: the
            // lowering its IR section needs lives behind `dump`, so the worker never reaches into
            // the compiler's own passes, and the supervisor process still sees nothing but the path
            // the dump was written to.
            if path.ends_with("worker.rs") {
                allowed.push("dump");
            }
            assert_allowed_external_crate_modules_in_file(&path, &allowed);
        }
    }

    #[test]
    fn production_lsp_isolates_compiler_analysis_in_a_bounded_worker() {
        let main =
            fs::read_to_string("crates/krusty-lsp/src/main.rs").expect("read LSP executable");
        assert!(main.contains("AnalysisWorker::spawn"));
        assert!(
            !main.contains("compiler_analysis::"),
            "the long-lived LSP supervisor must not run compiler analysis directly"
        );
        let worker =
            fs::read_to_string("crates/krusty-lsp/src/worker.rs").expect("read LSP worker");
        assert!(worker.contains("DEFAULT_ANALYSES_PER_WORKER"));
        assert!(worker.contains("self.analyses >= self.max_analyses"));
        assert!(worker.contains("ANALYSIS_TIMEOUT"));
        assert!(worker.contains("MAX_SOURCE_SET_BYTES"));
    }

    #[test]
    fn jvm_target_modules_use_only_jvm_side_dependencies() {
        let allowed = [
            "ast",
            "backend",
            "contracts",
            "diag",
            "fir",
            "frontend",
            "ir",
            "jvm",
            "klib",
            "kt_string",
            "libraries",
            "lru",
            "metadata",
            "module_symbols",
            "name_tree",
            "names",
            "plugins",
            "runtime",
            "spelling",
            "symbol_resolver",
            "symbol_source",
            "toolchain",
            "trace",
            "trace_compiler",
            "types",
            // Exact value-class declaration chains and target-parameterized traversal are common
            // semantics. Each backend still owns its carrier, boxing, storage, and emitted ABI.
            "value_classes",
            "wide_stack",
        ];
        for path in rust_files_under("src/jvm") {
            if path.ends_with("java_stub.rs") {
                let mut allowed = allowed.to_vec();
                allowed.push("java_source");
                assert_allowed_crate_modules_in_file(&path, &allowed);
            } else {
                assert_allowed_crate_modules_in_file(&path, &allowed);
            }
        }
    }

    #[test]
    fn method_parameter_identity_has_one_typed_boundary() {
        for path in [
            "src/ir/function_parameters.rs",
            "src/jvm/method_parameters.rs",
            "src/jvm/classfile/method_parameters.rs",
        ] {
            assert!(
                source_path(path).is_file(),
                "missing ownership module {path}"
            );
        }
        let emitter =
            fs::read_to_string(source_path("src/jvm/ir_emit.rs")).expect("read JVM emitter facade");
        assert!(!emitter.contains("fn declared_method_parameters"));
        assert!(!emitter.contains("fn declared_constructor_parameters"));
        assert!(!emitter.contains("internal.render() == \"kotlin/coroutines/Continuation\""));
        let classfile =
            fs::read_to_string(source_path("src/jvm/classfile.rs")).expect("read classfile facade");
        assert!(!classfile.contains("pub fn set_method_parameters"));

        for path in rust_files_under("src/jvm") {
            let text = fs::read_to_string(&path).expect("read JVM parameter consumer");
            let mut forbidden = vec![
                "format!(\"p{",
                "unwrap_or_else(|| format!(\"p",
                "parameter_names::legacy",
                ".param_names(",
            ];
            if !path.ends_with("parameter_names.rs") {
                forbidden.extend(["\"p1\"", "\"p2\""]);
            }
            for forbidden in forbidden {
                assert!(
                    !text.contains(forbidden),
                    "{} must project typed parameter identity at the JVM surface, not use `{forbidden}`",
                    path.display(),
                );
            }
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
    fn jvm_passes_do_not_rebuild_the_checked_classifier_hierarchy() {
        for path in ["src/jvm/backend.rs", "src/jvm/bridges.rs"] {
            let text = fs::read_to_string(path).expect("read JVM pass");
            for forbidden in [".applied_hierarchy(", ".supertype_internal_names_from("] {
                assert!(
                    !text.contains(forbidden),
                    "{path} must consume IrFile::classifier_hierarchies, not call `{forbidden}`"
                );
            }
        }
    }

    #[test]
    fn js_facade_has_no_crate_dependencies() {
        assert_allowed_crate_modules("src/js/mod.rs", &[]);
    }

    #[test]
    fn js_emitter_uses_only_ir_contract_dependencies() {
        assert_allowed_crate_modules("src/js/emit.rs", &["ir", "kt_string", "types"]);
    }

    #[test]
    fn js_backend_adapter_uses_only_common_backend_dependencies() {
        assert_allowed_crate_modules(
            "src/js/backend.rs",
            &[
                "backend",
                "compiler",
                "diag",
                "features",
                "frontend",
                "libraries",
                "source",
            ],
        );
    }

    /// A KLIB is the library format every non-JVM target reads, and the JVM backend already reads one
    /// for the common `expect` headers. The container reader is therefore core, and it stays core only
    /// as long as it depends on NOTHING else in the compiler: no target spelling, no symbol tables, no
    /// type engine. It hands out bytes and the packages they belong to; deciding what a declaration
    /// means is the caller's.
    #[test]
    fn klib_container_reader_depends_on_no_compiler_module() {
        assert_allowed_crate_modules("src/klib.rs", &[]);
    }

    #[test]
    fn native_facade_has_no_crate_dependencies() {
        assert_allowed_crate_modules("src/native/mod.rs", &[]);
    }

    #[test]
    fn the_native_runtime_sources_are_target_text_only() {
        // `runtime.rs` carries the runtime's C headers. It has no business knowing what a Kotlin
        // type or a compiler IR is.
        assert_allowed_crate_modules("src/native/runtime.rs", &[]);
    }

    #[test]
    fn the_class_model_uses_only_ir_contract_dependencies() {
        // `fir` is on this list for the identities common IR carries by value: a checked property
        // operation names a `PropertyId`, and an override edge names the `CallableId` it resolved.
        // Both are part of the IR contract, not a way back into the frontend.
        // `names` for the same reason `lower/statics.rs` has it: Kotlin's accessor-naming rule is
        // what common lowering applied when it named an abstract property's accessor, and reading
        // it from the same place is what lets the interface and its implementation agree on one
        // key for the member.
        assert_allowed_crate_modules("src/native/classes.rs", &["fir", "ir", "names", "types"]);
        assert_allowed_crate_modules("src/native/intrinsics.rs", &["types"]);
    }

    #[test]
    fn the_code_generator_uses_only_ir_contract_dependencies() {
        // `jvm` is GONE from both of these. It was here for one reason — the only symbol provider
        // read a JVM classpath, so a selected dependency declaration resolved through it — and the
        // generator now holds a `SemanticPlatform` and asks IT what it realized an identity as.
        // That contract is `libraries`, and it names no target.
        assert_allowed_crate_modules(
            "src/native/codegen/mod.rs",
            &["backend", "diag", "frontend", "libraries"],
        );
        // `fir` for the same reason `objects.rs` has it: the checked property and callable ids
        // the IR itself carries. Here it is the `ExternalPropertyId` a declining diagnostic names
        // the property by — the generator reads the id's name, never a declaration through it.
        assert_allowed_crate_modules(
            "src/native/codegen/lower.rs",
            &["fir", "ir", "libraries", "types"],
        );
        // `fir` only for the checked property and callable ids the IR itself carries. `names` for
        // the same reason `statics.rs` has it, below: a `super` access to a property arrives named
        // by its ACCESSOR, and this file recovers which property that is by deriving each
        // candidate's accessor name with Kotlin's own rule rather than by parsing the given one
        // back — the direction `IrSuperCallKind` documents, and the only one a `@JvmName`-mangled
        // accessor does not break.
        assert_allowed_crate_modules(
            "src/native/codegen/lower/objects.rs",
            &["fir", "ir", "names", "types"],
        );
        // `names` is Kotlin's own accessor-naming rule, which common lowering already applied when
        // it named a source-written accessor; reading it from the same place is what keeps the two
        // from drifting.
        assert_allowed_crate_modules(
            "src/native/codegen/lower/statics.rs",
            &["fir", "names", "types"],
        );
    }

    #[test]
    fn the_linker_knows_nothing_about_kotlin() {
        // Objects in, an executable out. A linker that imported the IR or the type system would be
        // a linker that had started making language decisions.
        assert_allowed_crate_modules("src/native/linker/mod.rs", &[]);
        assert_allowed_crate_modules("src/native/linker/elf.rs", &[]);
        assert_allowed_crate_modules("src/native/prebuilt.rs", &[]);
    }

    #[test]
    fn jvm_spellings_of_kotlin_builtins_stay_in_one_place() {
        // `kotlin.String` reaches the backend spelled `java/lang/String`, and a top-level function
        // reaches it owned by a file facade. Both are artifacts of reading signatures out of a JVM
        // jar, and both are normalized in `intrinsics.rs`. A second file learning to recognize
        // those spellings is how a temporary bridge becomes permanent.
        for path in rust_files_under("src/native") {
            if path.ends_with("intrinsics.rs") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read native source");
            for forbidden in ["java/lang", "java/util", "Kt\""] {
                assert!(
                    !text.contains(forbidden),
                    "{} spells a JVM provider detail (`{forbidden}`); normalize it in \
                     src/native/intrinsics.rs instead",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn fir_lower_facade_uses_only_common_lowering_dependencies() {
        assert_allowed_crate_modules("src/fir_lower/mod.rs", &["fir", "ir", "types"]);
    }

    #[test]
    fn checked_fir_lowering_uses_only_closed_semantic_handoff_dependencies() {
        for path in rust_files_under("src/fir_lower") {
            if path.ends_with("tests.rs") {
                continue;
            }
            // `wide_stack` is infrastructure, not a semantic dependency: a `stacker` wrapper and
            // the shared nesting bound. Lowering recurses over the checked nesting and must be
            // able to grow the stack to reach that bound, exactly as the checker does.
            assert_allowed_crate_modules_in_file(
                &path,
                &[
                    "fir",
                    "ir",
                    "names",
                    "trace",
                    "trace_compiler",
                    "types",
                    "wide_stack",
                ],
            );
        }
    }

    #[test]
    fn checked_fir_lowering_has_no_symbol_selection_entry_points() {
        for path in rust_files_under("src/fir_lower") {
            if path.ends_with("tests.rs") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read checked FIR lowerer");
            for forbidden in [
                "ModuleSymbols",
                "SymbolResolver",
                "fn resolve_",
                ".resolve_",
                ".prop_of(",
                ".method_of_name(",
                ".fun_by_params(",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{} must consume checked FIR selections, not use '{forbidden}'",
                    path.display(),
                );
            }
        }
    }

    #[test]
    fn checked_fir_lowering_does_not_format_jvm_inline_debug_names() {
        for path in rust_files_under("src/fir_lower") {
            let text = fs::read_to_string(&path).expect("read checked FIR lowerer");
            for forbidden in ["$iv", "_u24"] {
                assert!(
                    !text.contains(forbidden),
                    "{} must retain typed inline-local provenance; the JVM boundary owns `{forbidden}` formatting",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn checked_fir_lowering_does_not_invent_parameter_spellings() {
        for path in rust_files_under("src/fir_lower") {
            let text = fs::read_to_string(&path).expect("read checked FIR lowerer");
            for forbidden in [
                "format!(\"$capture",
                "format!(\"$this$",
                "format!(\"$context_receiver_",
                "\"$this$inline\"",
                "\"<this>\".to_",
                "format!(\"p{",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{} must publish typed parameter identity/provenance; the target owns `{forbidden}` formatting",
                    path.display(),
                );
            }
        }
    }

    #[test]
    fn module_symbols_uses_only_frontend_symbol_handoff_dependencies() {
        // `names` is a dependency-free leaf of Kotlin naming conventions (accessor spellings, the
        // package-vs-nesting internal-name split). Surfacing a top-level property needs its accessor
        // names, and re-deriving them here would fork the convention rather than share it.
        assert_allowed_crate_modules(
            "src/module_symbols.rs",
            &[
                "frontend",
                "libraries",
                "names",
                "symbol_source",
                "trace_compiler",
                "types",
            ],
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
    fn contract_ir_depends_only_on_ast_and_types() {
        // The shared contract IR must stay backend-agnostic: proto wire shapes live in
        // `jvm/metadata` + `metadata/builder`, call-site application in `resolve`.
        assert_allowed_crate_modules("src/contracts.rs", &["ast", "types"]);
    }

    #[test]
    fn semantic_library_contract_uses_only_symbol_source_and_type_dependencies() {
        // `spelling` is admitted on the same footing as `contracts`: a leaf data module over
        // `types` with no resolution or backend machinery behind it. A dependency's `typealias`
        // spellings have to cross this boundary — they are decoded from its metadata and consumed
        // when resolving a use site — and they are inert data, which is what this budget protects
        // against, not what it forbids.
        assert_allowed_crate_modules(
            "src/libraries.rs",
            &[
                "contracts",
                "fir",
                "kt_string",
                "name_tree",
                "spelling",
                "symbol_source",
                "types",
            ],
        );
    }

    #[test]
    fn native_plugins_use_only_plugin_and_ir_contract_dependencies() {
        assert_allowed_crate_modules_in_tree(
            "src/plugins",
            &[
                "ast",
                "diag",
                "ir",
                "kt_string",
                "libraries",
                "lexer",
                "names",
                "parser",
                "plugins",
                "trace_compiler",
                "types",
            ],
        );
    }

    #[test]
    fn frontend_tools_use_only_their_declared_frontend_handoff_dependencies() {
        assert_allowed_crate_modules("src/bin/check.rs", &["diag", "frontend"]);
        assert_allowed_crate_modules(
            "src/bin/blockers.rs",
            &["conformance", "diag", "frontend", "lexer", "parser"],
        );
        assert_allowed_crate_modules(
            "src/bin/bytediff.rs",
            &[
                "compiler",
                "conformance",
                "diag",
                "features",
                "frontend",
                "jvm",
                "source",
            ],
        );
        assert_allowed_crate_modules(
            "src/bin/survey.rs",
            &[
                "ast",
                // The frontend census drives the PRODUCTION two-pass pipeline
                // (`compiler::check_frontend_only` over `source::SourceInput`s) rather than a
                // survey-local imitation of it, so a conformance number cannot drift from what
                // ships. Emission stays out: the census attaches no backend.
                "compiler",
                "conformance",
                "diag",
                "features",
                "frontend",
                "ir",
                "jvm",
                "lexer",
                "parser",
                "source",
                "toolchain",
                "trace_compiler",
                "types",
            ],
        );
    }

    #[test]
    fn integration_tests_use_only_public_compiler_layer_dependencies() {
        assert_allowed_external_crate_modules_in_tree(
            "tests",
            &[
                "ast",
                "backend",
                "compiler",
                "conformance",
                "dhat",
                "diag",
                "features",
                "frontend",
                "ir",
                "js",
                "jvm",
                "klib",
                "lexer",
                "native",
                "libraries",
                "metadata",
                "parser",
                "plugins",
                "source",
                "symbol_resolver",
                "symbol_source",
                "toolchain",
                "trace_compiler",
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

    /// The tracing macro is not a dependency in the sense these budgets are about.
    ///
    /// A budget says which parts of the compiler a file may KNOW about — which layer's concepts it
    /// is allowed to reason in. `trace_compiler!` carries no concepts: it is off by default, where
    /// it compiles to nothing at all, and `CLAUDE.md` names it as THE way to emit diagnostics from
    /// compiler code. Counting it would mean every file that ever traces has to widen its budget to
    /// say so, which tells a reader nothing and makes the real entries harder to see.
    const CROSS_CUTTING: &[&str] = &["trace_compiler"];

    fn collect_path_module(path: &syn::Path, modules: &mut BTreeSet<String>, roots: &[&str]) {
        let mut segments = path.segments.iter();
        if segments
            .next()
            .is_some_and(|segment| is_crate_root(segment, roots))
        {
            if let Some(module) = segments.next() {
                let module = module.ident.to_string();
                if !CROSS_CUTTING.contains(&module.as_str()) {
                    modules.insert(module);
                }
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
