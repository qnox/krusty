//! Exact statement-local alias dependencies of ordinary local-class headers.
//!
//! Ordinary bodies do not enter signature inference, but Pass 1 must distinguish a representable
//! module-level spelling from a local alias that changes a primary constructor/property argument
//! shape. This structural walk records only that identity-bearing header dependency; it does not
//! extract body expressions or make the local classifier a signature root.

use super::*;
use crate::fir::DeclarationKind;

impl SignatureConstraintExtractor {
    pub(super) fn capture_ordinary_local_alias_headers(
        &mut self,
        file: &File,
        source: SourceFileId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        let roots = file
            .decls
            .iter()
            .copied()
            .filter_map(|declaration| {
                let owner = match file.decl(declaration) {
                    crate::ast::Decl::Fun(function) => {
                        self.source_functions.get(&function.span).copied()
                    }
                    crate::ast::Decl::Class(classifier) => {
                        self.source_classifiers.get(&classifier.span).copied()
                    }
                    crate::ast::Decl::Property(property) => {
                        self.source_stubs.get(&property.span).map(|stub| stub.id)
                    }
                }?;
                let mut roots = Vec::new();
                file.any_decl_expr(declaration, &mut |expression| {
                    roots.push(expression);
                    false
                });
                Some(roots.into_iter().map(move |expression| (owner, expression)))
            })
            .flatten()
            .collect::<Vec<_>>();
        for (owner, root) in roots {
            let scope = self.graph.add_scope(SignatureScope { owner, source });
            self.lexical_types.push(HashMap::default());
            self.capture_local_alias_headers_expression(file, root, scope, origin);
            self.lexical_types.pop();
        }
    }

    fn capture_local_alias_headers_expression(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        if let Expr::Block { stmts, trailing } = file.expr(expression) {
            self.lexical_types.push(HashMap::default());
            for statement in stmts {
                self.capture_local_alias_headers_statement(file, *statement, scope, origin);
            }
            if let Some(trailing) = trailing {
                self.capture_local_alias_headers_expression(file, *trailing, scope, origin);
            }
            self.lexical_types.pop();
            return;
        }
        let mut expressions = Vec::new();
        let mut statements = Vec::new();
        file.any_child_expr(
            expression,
            &mut |child| {
                expressions.push(child);
                false
            },
            &mut |child| {
                statements.push(child);
                false
            },
        );
        for child in expressions {
            self.capture_local_alias_headers_expression(file, child, scope, origin);
        }
        for child in statements {
            self.capture_local_alias_headers_statement(file, child, scope, origin);
        }
    }

    fn capture_local_alias_headers_statement(
        &mut self,
        file: &File,
        statement: crate::ast::StmtId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) {
        match file.stmt(statement) {
            Stmt::LocalTypeAlias(alias) => {
                let root =
                    self.compact_local_type_template(&alias.target, scope, &alias.type_params);
                let template =
                    self.graph
                        .add_local_type_alias_template(root, alias.type_params.len(), scope);
                self.lexical_types
                    .last_mut()
                    .expect("a local alias header walk retains its block scope")
                    .insert(
                        alias.name.clone().into_boxed_str(),
                        CompactLexicalType::Alias(CompactLocalTypeAlias {
                            arity: alias.type_params.len(),
                            template,
                        }),
                    );
            }
            Stmt::LocalClass(_) => {
                let classifier =
                    file.local_class_decls
                        .get(&statement)
                        .copied()
                        .and_then(|declaration| match file.decl(declaration) {
                            crate::ast::Decl::Class(class) => self
                                .source_classifiers
                                .get(&class.span)
                                .copied()
                                .map(|stable| (class, stable)),
                            crate::ast::Decl::Fun(_) | crate::ast::Decl::Property(_) => None,
                        });
                if let Some((class, stable)) = classifier {
                    if let Some(&constructor) = self.source_primary_constructors.get(&class.span) {
                        for parameter in &class.props {
                            let expression = self.compact_type(&parameter.ty, scope, origin);
                            self.graph
                                .record_shape_changing_local_alias_header(constructor, expression);
                            if parameter.is_property {
                                if let Some(property) = self
                                    .source_stubs
                                    .get(&parameter.span)
                                    .filter(|stub| stub.kind == DeclarationKind::Property)
                                {
                                    self.graph.record_shape_changing_local_alias_header(
                                        property.id,
                                        expression,
                                    );
                                }
                            }
                        }
                    }
                    self.lexical_types
                        .last_mut()
                        .expect("a local class header walk retains its block scope")
                        .insert(
                            class
                                .name
                                .rsplit('.')
                                .next()
                                .unwrap_or(&class.name)
                                .to_owned()
                                .into_boxed_str(),
                            CompactLexicalType::Classifier(stable),
                        );
                }
            }
            _ => {
                let mut expressions = Vec::new();
                file.any_child_stmt(statement, &mut |child| {
                    expressions.push(child);
                    false
                });
                for child in expressions {
                    self.capture_local_alias_headers_expression(file, child, scope, origin);
                }
            }
        }
    }
}
