//! Bounded Pass-2 declaration parsing.
//!
//! The lexer token stream and parser cursor live for one source, but parser arenas live for only
//! one top-level declaration unit. The consumer receives that unit synchronously; after it returns,
//! the arenas are dropped before parsing resumes. No Pass-1 source coordinate selects a unit.

use super::*;

#[derive(Default)]
struct DeclarationPrelude {
    package: Option<String>,
    imports: Vec<ImportPath>,
    detached_import_types: Vec<TypeRef>,
    detached_import_suppressions: std::collections::HashMap<u32, Vec<String>>,
}

impl DeclarationPrelude {
    fn learn_file_prefix(&mut self, file: &File) {
        self.package.clone_from(&file.package);
        self.imports.clone_from(&file.import_paths);
        self.detached_import_types = file
            .detached_type_refs
            .iter()
            .filter(|reference| reference.is_import())
            .cloned()
            .collect();
        let import_offsets = self
            .detached_import_types
            .iter()
            .map(|reference| reference.span.lo)
            .collect::<std::collections::HashSet<_>>();
        self.detached_import_suppressions = file
            .detached_type_ref_suppressions
            .iter()
            .filter(|(offset, _)| import_offsets.contains(offset))
            .map(|(offset, suppressions)| (*offset, suppressions.clone()))
            .collect();
    }

    fn fresh_file(&self) -> File {
        File {
            package: self.package.clone(),
            import_paths: self.imports.clone(),
            detached_type_refs: self.detached_import_types.clone(),
            detached_type_ref_suppressions: self.detached_import_suppressions.clone(),
            ..File::default()
        }
    }
}

fn finish_unit(file: &mut File, source: &str, features: &LangFeatures, diags: &mut DiagSink) {
    apply_file_features(file, features);
    hoist_local_classes(file, None);
    fixup_parenless_base_classes(file);
    fill_class_decl_lines(file, source);
    expand_fun_type_aliases(file);
    if !diags.has_errors() {
        if let Err(error) = file.validate_integrity(source) {
            diags.error(Span::new(0, 0), format!("invalid parser AST: {error}"));
        }
    }
}

/// Parse a Kotlin source once and synchronously visit each top-level declaration AST.
pub(crate) fn visit_declaration_units_with_features(
    source: &str,
    tokens: &[Token],
    diags: &mut DiagSink,
    features: &LangFeatures,
    mut visit: impl FnMut(File, &mut DiagSink),
) {
    let mut parser = Parser::new(source, tokens, diags, features, false);
    let mut prelude = DeclarationPrelude::default();
    crate::wide_stack::on_wide_stack(|| {
        parser.parse_file_with_declaration_sink(&mut |parser| {
            prelude.learn_file_prefix(&parser.file);
            let mut unit = std::mem::replace(&mut parser.file, prelude.fresh_file());
            // `parenthesized_expressions` is indexed by the current expression arena. Bounded
            // units deliberately restart that arena at zero, so keeping the previous unit's IDs
            // would make unrelated later expressions appear parenthesized. In particular, a stale
            // ID can turn `consume(x) { ... }` into an invocation of `consume(x)` instead of a call
            // with a trailing lambda.
            parser.parenthesized_expressions.clear();
            // Module aliases are stable semantic headers in the finalized declaration index.
            // Keeping parser `TypeRef` trees beside that index would retain Pass-1 syntax across
            // the pass boundary, so a bounded unit deliberately carries only aliases declared
            // inside that unit. The Pass-2 checker resolves module aliases from the index.
            finish_unit(&mut unit, source, features, parser.diags);
            visit(unit, parser.diags);
        });
    });
    parser.skip_newlines();
    if !parser.at(TokenKind::Eof) || parser.i + 1 != parser.t.len() {
        parser.diags.error(
            parser.tok().span,
            "parser did not consume every non-trivia token",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visits_and_drops_one_top_level_declaration_arena_at_a_time() {
        let source = "fun first() = 1\nclass C { fun member() = 2 }\nval last = 3";
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let mut units = Vec::new();
        visit_declaration_units_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &LangFeatures::new(),
            |file, _| {
                units.push((file.decls.len(), file.expr_arena.len()));
            },
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(units.len(), 3);
        assert_eq!(
            units.iter().map(|unit| unit.0).collect::<Vec<_>>(),
            [1, 1, 1]
        );
    }

    #[test]
    fn bounded_units_keep_the_full_file_language_policy() {
        let source =
            "// LANGUAGE: +EnumEntries -PrioritizedEnumEntries\nfun first() = 1\nfun second() = 2";
        let features = LangFeatures::from_source(source);
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let mut policies = Vec::new();

        visit_declaration_units_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &features,
            |file, _| {
                policies.push((file.enum_entries_enabled, file.prioritized_enum_entries));
            },
        );

        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(policies, [(true, false), (true, false)]);
    }

    #[test]
    fn local_classifier_hoisting_does_not_merge_adjacent_bounded_units() {
        let source = "fun first() { class Local { inner class Nested } }\nfun second() = 2";
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let mut units = Vec::new();
        visit_declaration_units_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &LangFeatures::new(),
            |file, _| units.push(file),
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(units.len(), 2);
        assert!(units.iter().all(|file| {
            file.decls
                .iter()
                .filter(|declaration| !file.is_local_declaration(**declaration))
                .count()
                == 1
        }));
    }

    #[test]
    fn recycled_expression_ids_do_not_inherit_parentheses_from_the_previous_unit() {
        let source = "fun seed(x: Any) = if (true) (x as String) else \"\"\n\
                      fun target(b: Boolean) = consume(b) { it }";
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let mut target = None;
        visit_declaration_units_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &LangFeatures::new(),
            |file, _| {
                if file.decls.iter().any(|declaration| {
                    matches!(file.decl(*declaration), Decl::Fun(function) if function.name == "target")
                }) {
                    target = Some(file);
                }
            },
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        let target = target.expect("target declaration unit");
        let function = target
            .decls
            .iter()
            .find_map(|&declaration| match target.decl(declaration) {
                Decl::Fun(function) if function.name == "target" => Some(function),
                _ => None,
            })
            .expect("target function");
        let FunBody::Expr(root) = function.body else {
            panic!("target must have an expression body")
        };
        let Expr::Call { callee, args } = target.expr(root) else {
            panic!("trailing lambda must remain part of the consume call")
        };
        assert!(matches!(target.expr(*callee), Expr::Name(name) if name == "consume"));
        assert_eq!(args.len(), 2);
        assert!(matches!(target.expr(args[1]), Expr::Lambda { .. }));
        assert!(target.call_has_trailing_lambda.contains(&root.0));
    }

    #[test]
    fn declaration_annotation_references_do_not_leak_into_the_next_unit() {
        let source =
            "import sample.Marker as Imported\n@Imported fun first() = 1\nfun second() = 2";
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let mut units = Vec::new();
        visit_declaration_units_with_features(
            source,
            &tokens,
            &mut diagnostics,
            &LangFeatures::new(),
            |file, _| units.push(file),
        );
        assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
        assert_eq!(units.len(), 2);
        assert!(units[0]
            .detached_type_refs
            .iter()
            .any(|reference| !reference.is_import() && reference.name == "Imported"));
        assert!(units[1].detached_type_refs.iter().all(TypeRef::is_import));
    }
}
