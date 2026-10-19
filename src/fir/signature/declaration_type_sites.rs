//! Identity bridge for explicit declaration types duplicated in two compact arenas.
//!
//! Graph `HeaderTypeId`s and declaration-header `HeaderTypeId`s are separate identity domains.
//! This owner records explicit structural edges; callers never reinterpret their raw numbers.

use super::*;

/// Compact explicit types captured while a body-local declaration's lexical type rung exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplicitSignatureTypes {
    pub parameters: OperandRange,
    pub result: Option<SigExprId>,
    pub getter_result: Option<SigExprId>,
    pub receiver: Option<SigExprId>,
    pub storage: Option<SigExprId>,
}

/// Parent types captured while a body-local classifier's lexical type rung exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplicitClassifierParents {
    pub superclass: Option<SigExprId>,
    pub supertypes: OperandRange,
}

#[derive(Default)]
pub(super) struct DeclarationTypeSites {
    by_graph_type: HashMap<HeaderTypeId, Vec<HeaderTypeId>>,
    by_expression: HashMap<SigExprId, Vec<HeaderTypeId>>,
    declaration_sites: HashSet<HeaderTypeId>,
}

impl DeclarationTypeSites {
    pub(super) fn bind(
        &mut self,
        nodes: &[SigExpr],
        graph_syntax: &HeaderSyntaxArena,
        expression: SigExprId,
        declaration_syntax: HeaderTypeId,
    ) {
        let expression_sites = self.by_expression.entry(expression).or_default();
        if !expression_sites.contains(&declaration_syntax) {
            expression_sites.push(declaration_syntax);
        }
        self.declaration_sites.insert(declaration_syntax);
        match nodes.get(expression.raw() as usize).copied() {
            Some(SigExpr::Type { syntax, .. } | SigExpr::ContextualType { syntax, .. }) => {
                if graph_syntax.written_type_id(syntax) == syntax {
                    let syntax_sites = self.by_graph_type.entry(syntax).or_default();
                    if !syntax_sites.contains(&declaration_syntax) {
                        syntax_sites.push(declaration_syntax);
                    }
                }
            }
            Some(SigExpr::Nullable(inner) | SigExpr::NonNullable(inner)) => {
                self.bind(nodes, graph_syntax, inner, declaration_syntax);
            }
            _ => {}
        }
    }

    pub(super) fn declaration_syntaxes_for_expression(
        &self,
        expression: SigExprId,
    ) -> &[HeaderTypeId] {
        self.by_expression
            .get(&expression)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(super) fn declaration_syntaxes_for_type(&self, syntax: HeaderTypeId) -> &[HeaderTypeId] {
        self.by_graph_type
            .get(&syntax)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(super) fn contains_declaration_site(&self, syntax: HeaderTypeId) -> bool {
        self.declaration_sites.contains(&syntax)
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.by_graph_type
            .values()
            .map(|sites| {
                std::mem::size_of::<(HeaderTypeId, Vec<HeaderTypeId>)>()
                    + sites.len() * std::mem::size_of::<HeaderTypeId>()
            })
            .sum::<usize>()
            + self
                .by_expression
                .values()
                .map(|sites| {
                    std::mem::size_of::<(SigExprId, Vec<HeaderTypeId>)>()
                        + sites.len() * std::mem::size_of::<HeaderTypeId>()
                })
                .sum::<usize>()
            + self.declaration_sites.len() * std::mem::size_of::<HeaderTypeId>()
    }
}

impl SignatureGraph {
    pub(crate) fn add_type_syntax(&mut self, ty: &crate::ast::TypeRef) -> HeaderTypeId {
        self.type_syntax.add_type(ty, &mut self.type_names)
    }

    pub(crate) fn transient_type_ref(&self, id: HeaderTypeId) -> Option<crate::ast::TypeRef> {
        self.type_syntax.transient_type_ref(id, &self.type_names)
    }

    pub(crate) fn transient_written_type_ref(
        &self,
        id: HeaderTypeId,
    ) -> Option<crate::ast::TypeRef> {
        self.type_syntax
            .transient_written_type_ref(id, &self.type_names)
    }

    pub(super) fn add_type_syntax_with_source_spelling(
        &mut self,
        ty: &crate::ast::TypeRef,
        written: &crate::ast::TypeRef,
    ) -> HeaderTypeId {
        self.type_syntax
            .add_type_with_source_spelling(ty, written, &mut self.type_names)
    }

    pub(crate) fn written_type_node(
        &self,
        id: HeaderTypeId,
    ) -> Option<(HeaderTypeId, bool, Vec<HeaderTypeId>)> {
        self.type_syntax.written_type_node(id)
    }

    pub(crate) fn written_type_id(&self, id: HeaderTypeId) -> HeaderTypeId {
        self.type_syntax.written_type_id(id)
    }

    pub(crate) fn expression_type_syntax(&self, expression: SigExprId) -> Option<HeaderTypeId> {
        match self.expr(expression)? {
            SigExpr::Type { syntax, .. } | SigExpr::ContextualType { syntax, .. } => Some(syntax),
            SigExpr::Nullable(inner) | SigExpr::NonNullable(inner) => {
                self.expression_type_syntax(inner)
            }
            _ => None,
        }
    }

    pub(crate) fn bind_declaration_type_syntax(
        &mut self,
        expression: SigExprId,
        declaration_syntax: HeaderTypeId,
    ) {
        self.declaration_type_sites.bind(
            &self.nodes,
            &self.type_syntax,
            expression,
            declaration_syntax,
        );
    }

    pub(crate) fn declaration_expression_syntaxes(&self, expression: SigExprId) -> &[HeaderTypeId] {
        self.declaration_type_sites
            .declaration_syntaxes_for_expression(expression)
    }

    pub(crate) fn declaration_type_syntaxes(&self, syntax: HeaderTypeId) -> &[HeaderTypeId] {
        self.declaration_type_sites
            .declaration_syntaxes_for_type(syntax)
    }

    pub(crate) fn declaration_type_site_is_bound(&self, syntax: HeaderTypeId) -> bool {
        self.declaration_type_sites
            .contains_declaration_site(syntax)
    }
}
