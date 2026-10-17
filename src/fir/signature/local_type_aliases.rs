//! Statement-local type aliases retained as declaration-site semantic applications.
//!
//! The alias target and its use-site arguments deliberately remain separate. Resolving a parser-
//! substituted `TypeRef` would bind target classifier spellings in the use scope and would resolve
//! every repeated formal occurrence again. The compact graph instead resolves each written
//! argument once, then binds those semantic results as the target's formals in its declaration
//! scope.

use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LocalTypeAliasTemplateId(u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LocalTypeTemplateNodeId(u32);

#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalTypeTemplateFlags {
    pub(crate) nullable: bool,
    pub(crate) definitely_non_null: bool,
    pub(crate) in_projection: bool,
    pub(crate) out_projection: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum LocalTypeTemplateTarget {
    Formal(u32),
    Star,
    Syntax {
        syntax: HeaderTypeId,
        scope: SignatureScopeId,
        child_formals: Box<[SigNameId]>,
    },
    Classifier {
        declaration: DeclarationId,
        scope: SignatureScopeId,
    },
    Alias(LocalTypeAliasTemplateId),
}

#[derive(Clone, Debug)]
pub(crate) struct LocalTypeTemplateNode {
    pub(crate) target: LocalTypeTemplateTarget,
    pub(crate) children: Box<[LocalTypeTemplateNodeId]>,
    pub(crate) flags: LocalTypeTemplateFlags,
}

#[derive(Clone, Debug)]
pub(crate) struct LocalTypeAliasTemplate {
    pub(crate) root: LocalTypeTemplateNodeId,
    pub(crate) formals: Box<[SigNameId]>,
}

#[derive(Clone, Debug)]
pub(crate) struct WrittenTypeApplication {
    pub(crate) target: HeaderTypeId,
    pub(crate) declaration_scope: SignatureScopeId,
    pub(crate) formals: Box<[SigNameId]>,
    pub(crate) arguments: OperandRange,
    pub(crate) written_argument_indices: Box<[u32]>,
    pub(crate) bind_root: bool,
    pub(crate) local_alias: Option<LocalTypeAliasTemplateId>,
    pub(crate) use_flags: Option<LocalTypeTemplateFlags>,
}

impl SignatureGraph {
    pub(crate) fn add_local_type_alias_application(
        &mut self,
        alias: LocalTypeAliasTemplateId,
        written: &crate::ast::TypeRef,
        arguments: impl IntoIterator<Item = SigExprId>,
        use_scope: SignatureScopeId,
        origin: OriginId,
    ) -> SigExprId {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        let syntax = self.add_type_syntax(written);
        let expression = self.add_expr(SigExpr::Type {
            syntax,
            scope: use_scope,
            origin,
        });
        let arguments = self.add_operands(arguments.iter().copied());
        self.written_type_applications.insert(
            expression,
            WrittenTypeApplication {
                target: syntax,
                declaration_scope: use_scope,
                formals: Box::new([]),
                arguments,
                written_argument_indices: (0..arguments.len).collect::<Vec<_>>().into_boxed_slice(),
                bind_root: false,
                local_alias: Some(alias),
                use_flags: Some(LocalTypeTemplateFlags {
                    nullable: written.nullable(),
                    definitely_non_null: written.definitely_non_null(),
                    in_projection: written.in_projection(),
                    out_projection: written.out_projection(),
                }),
            },
        );
        expression
    }

    pub(crate) fn add_written_type_application(
        &mut self,
        target: &crate::ast::TypeRef,
        written: &crate::ast::TypeRef,
        declaration_scope: SignatureScopeId,
        formals: &[String],
        arguments: impl IntoIterator<Item = SigExprId>,
        written_argument_indices: impl IntoIterator<Item = u32>,
        use_scope: SignatureScopeId,
        origin: OriginId,
        bind_root: bool,
    ) -> SigExprId {
        // Ordinary written generic/function types use a shallow symbolic target. Local aliases do
        // not enter this path: their declaration-owned template was captured before any use and
        // `add_local_type_alias_application` retains only its stable template identity.
        let mut target = target.clone();
        target.span = written.span;
        target.set_nullable(target.nullable() || written.nullable());
        target.set_projection(
            target.in_projection() || written.in_projection(),
            target.out_projection() || written.out_projection(),
        );
        let syntax = self.add_type_syntax_with_source_spelling(&target, written);
        let formals = formals
            .iter()
            .map(|formal| self.intern_name(formal))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let arguments = self.add_operands(arguments);
        let expression = self.add_expr(SigExpr::Type {
            syntax,
            scope: use_scope,
            origin,
        });
        self.written_type_applications.insert(
            expression,
            WrittenTypeApplication {
                target: syntax,
                declaration_scope,
                formals,
                arguments,
                written_argument_indices: written_argument_indices
                    .into_iter()
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                bind_root,
                local_alias: None,
                use_flags: None,
            },
        );
        expression
    }

    pub(crate) fn written_type_application(
        &self,
        expression: SigExprId,
    ) -> Option<&WrittenTypeApplication> {
        self.written_type_applications.get(&expression)
    }

    pub(crate) fn written_type_application_arguments(
        &self,
        expression: SigExprId,
    ) -> Option<impl Iterator<Item = (SigExprId, u32)> + '_> {
        let application = self.written_type_application(expression)?;
        Some(
            self.operands(application.arguments)
                .iter()
                .copied()
                .zip(application.written_argument_indices.iter().copied()),
        )
    }

    pub(super) fn written_type_application_payload_bytes(&self) -> usize {
        let applications = self
            .written_type_applications
            .values()
            .map(|application| {
                std::mem::size_of::<(SigExprId, WrittenTypeApplication)>()
                    + application.formals.len() * std::mem::size_of::<SigNameId>()
                    + application.written_argument_indices.len() * std::mem::size_of::<u32>()
            })
            .sum::<usize>();
        let templates = self.local_type_alias_templates.len()
            * std::mem::size_of::<LocalTypeAliasTemplate>()
            + self
                .local_type_alias_templates
                .iter()
                .map(|template| template.formals.len() * std::mem::size_of::<SigNameId>())
                .sum::<usize>();
        let nodes = self.local_type_template_nodes.len()
            * std::mem::size_of::<LocalTypeTemplateNode>()
            + self
                .local_type_template_nodes
                .iter()
                .map(|node| {
                    node.children.len() * std::mem::size_of::<LocalTypeTemplateNodeId>()
                        + match &node.target {
                            LocalTypeTemplateTarget::Syntax { child_formals, .. } => {
                                child_formals.len() * std::mem::size_of::<SigNameId>()
                            }
                            _ => 0,
                        }
                })
                .sum::<usize>();
        applications + templates + nodes
    }

    pub(crate) fn add_local_type_template_node(
        &mut self,
        node: LocalTypeTemplateNode,
    ) -> LocalTypeTemplateNodeId {
        let id = LocalTypeTemplateNodeId(next_id(
            self.local_type_template_nodes.len(),
            "local type template nodes",
        ));
        self.local_type_template_nodes.push(node);
        id
    }

    pub(crate) fn add_local_type_alias_template(
        &mut self,
        root: LocalTypeTemplateNodeId,
        formal_count: usize,
    ) -> LocalTypeAliasTemplateId {
        let id = LocalTypeAliasTemplateId(next_id(
            self.local_type_alias_templates.len(),
            "local type alias templates",
        ));
        let formals = (0..formal_count)
            .map(|ordinal| self.intern_name(&format!("\0local-alias-{}-{ordinal}", id.0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        self.local_type_alias_templates
            .push(LocalTypeAliasTemplate { root, formals });
        id
    }

    pub(crate) fn local_type_template_node(
        &self,
        id: LocalTypeTemplateNodeId,
    ) -> Option<&LocalTypeTemplateNode> {
        self.local_type_template_nodes.get(id.0 as usize)
    }

    pub(crate) fn local_type_alias_template(
        &self,
        id: LocalTypeAliasTemplateId,
    ) -> Option<&LocalTypeAliasTemplate> {
        self.local_type_alias_templates.get(id.0 as usize)
    }

    pub(crate) fn local_type_alias_template_ids(
        &self,
    ) -> impl Iterator<Item = LocalTypeAliasTemplateId> + '_ {
        (0..self.local_type_alias_templates.len()).map(|index| {
            LocalTypeAliasTemplateId(
                u32::try_from(index).expect("too many local type alias templates"),
            )
        })
    }
}
