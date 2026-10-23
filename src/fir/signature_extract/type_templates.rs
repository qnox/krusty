//! Structural capture of written types and statement-local type-alias templates.

use super::*;
use crate::fir::{
    LocalTypeTemplateFlags, LocalTypeTemplateNode, LocalTypeTemplateNodeId, LocalTypeTemplateTarget,
};

impl SignatureConstraintExtractor {
    pub(super) fn compact_type(
        &mut self,
        ty: &crate::ast::TypeRef,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> SigExprId {
        // The parser expands source aliases for the ordinary AST, but compact resolution must bind
        // the written alias itself. Its declaration owns the already-resolved expansion; resolving
        // the expanded target in this use scope could redirect a declaration-site classifier to a
        // later same-named local declaration. Capture the parser sidecar into graph syntax now,
        // while source spans are still only an ingestion key.
        let written = self
            .source_type_alias_spellings
            .get(&ty.span)
            .cloned()
            .unwrap_or_else(|| ty.clone());
        let ty = &written;
        if ty.is_star_projection() {
            let star = Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")));
            return self.graph.add_expr(SigExpr::Known(
                ResolvedTy::new(star).expect("a star projection is a publishable type argument"),
            ));
        }
        if ty.fun_params.is_empty() && ty.name != "<fun>" {
            if let Some(alias) = self
                .lexical_types
                .iter()
                .rev()
                .find_map(|types| types.get(ty.name.as_str()))
                .and_then(|binding| match binding {
                    CompactLexicalType::Alias(alias) => Some(alias.clone()),
                    CompactLexicalType::Classifier(_) => None,
                })
                .filter(|alias| alias.arity == ty.targs.len())
            {
                let arguments = ty
                    .targs
                    .iter()
                    .map(|argument| self.compact_type(argument, scope, origin))
                    .collect::<Vec<_>>();
                let origin = origin(ty.span);
                return self.graph.add_local_type_alias_application(
                    alias.template,
                    ty,
                    arguments,
                    scope,
                    origin,
                );
            }
        }
        let mut target = ty.clone();
        let mut arguments = Vec::new();
        let mut formals = Vec::new();
        let mut written_argument_indices = Vec::new();
        let mut bind_child =
            |child: &TypeRef, target_child: &mut TypeRef, index: u32, this: &mut Self| {
                let formal = format!("\0signature-type-argument-{}", arguments.len());
                arguments.push(this.compact_type(child, scope, origin));
                formals.push(formal.clone());
                written_argument_indices.push(index);
                *target_child = TypeRef {
                    name: formal,
                    flags: TrFlags::default(),
                    arg: None,
                    targs: Vec::new(),
                    span: child.span,
                    fun_params: Box::new([]),
                    fun_context_count: 0,
                    qualifier_arguments: Default::default(),
                };
            };
        if ty.is_function_type_syntax() {
            for (index, (child, target_child)) in ty
                .fun_params
                .iter()
                .zip(target.fun_params.iter_mut())
                .enumerate()
            {
                bind_child(child, target_child, index as u32, self);
            }
            if let (Some(child), Some(target_child)) =
                (ty.arg.as_deref(), target.arg.as_deref_mut())
            {
                bind_child(child, target_child, ty.fun_params.len() as u32, self);
            }
        } else {
            for (index, (child, target_child)) in
                ty.targs.iter().zip(target.targs.iter_mut()).enumerate()
            {
                bind_child(child, target_child, index as u32, self);
            }
            if let (Some(child), Some(target_child)) =
                (ty.arg.as_deref(), target.arg.as_deref_mut())
            {
                bind_child(child, target_child, ty.targs.len() as u32, self);
            }
        }
        if !arguments.is_empty() {
            let root_origin = origin(ty.span);
            return self.graph.add_written_type_application(
                &target,
                ty,
                scope,
                &formals,
                arguments,
                written_argument_indices,
                scope,
                root_origin,
                true,
            );
        }
        let visible = self
            .lexical_types
            .iter()
            .flat_map(HashMap::iter)
            .map(|(name, binding)| (name.clone(), binding.clone()))
            .collect::<HashMap<_, _>>();
        if ty.arg.is_none()
            && ty.targs.is_empty()
            && ty.fun_params.is_empty()
            && !ty.in_projection()
            && !ty.out_projection()
            && !ty.is_star_projection()
        {
            if let Some(CompactLexicalType::Classifier(declaration)) = visible.get(ty.name.as_str())
            {
                let mut result = self.graph.add_expr(SigExpr::ClassifierType {
                    declaration: *declaration,
                    scope,
                });
                if ty.nullable() {
                    result = self.graph.add_expr(SigExpr::Nullable(result));
                }
                if ty.definitely_non_null() {
                    result = self.graph.add_expr(SigExpr::NonNullable(result));
                }
                return result;
            }
        }
        let syntax = self.graph.add_type_syntax(ty);
        let origin = origin(ty.span);
        self.graph.add_expr(SigExpr::Type {
            syntax,
            scope,
            origin,
        })
    }

    pub(super) fn compact_local_type_template(
        &mut self,
        ty: &TypeRef,
        scope: SignatureScopeId,
        formals: &[String],
    ) -> LocalTypeTemplateNodeId {
        // As with an explicit compact type, retain a source alias as the declaration-site lookup
        // input. The local template binds that syntax once in the alias declaration's scope.
        let written = self
            .source_type_alias_spellings
            .get(&ty.span)
            .cloned()
            .unwrap_or_else(|| ty.clone());
        let ty = &written;
        let source = self.graph.add_type_syntax(ty);
        let flags = LocalTypeTemplateFlags {
            nullable: ty.nullable(),
            definitely_non_null: ty.definitely_non_null(),
            in_projection: ty.in_projection(),
            out_projection: ty.out_projection(),
        };
        if ty.is_star_projection() {
            return self
                .graph
                .add_local_type_template_node(LocalTypeTemplateNode {
                    source,
                    target: LocalTypeTemplateTarget::Star,
                    children: Box::new([]),
                    flags,
                });
        }
        if ty.arg.is_none() && ty.targs.is_empty() && ty.fun_params.is_empty() {
            if let Some(index) = formals.iter().position(|formal| formal == &ty.name) {
                return self
                    .graph
                    .add_local_type_template_node(LocalTypeTemplateNode {
                        source,
                        target: LocalTypeTemplateTarget::Formal(index as u32),
                        children: Box::new([]),
                        flags,
                    });
            }
        }
        let lexical = self
            .lexical_types
            .iter()
            .rev()
            .find_map(|types| types.get(ty.name.as_str()))
            .cloned();
        let children = if ty.is_function_type_syntax() {
            ty.fun_params
                .iter()
                .chain(ty.arg.iter().map(Box::as_ref))
                .map(|child| self.compact_local_type_template(child, scope, formals))
                .collect::<Vec<_>>()
        } else {
            ty.targs
                .iter()
                .chain(ty.arg.iter().map(Box::as_ref))
                .map(|child| self.compact_local_type_template(child, scope, formals))
                .collect::<Vec<_>>()
        }
        .into_boxed_slice();
        let target = match lexical {
            Some(CompactLexicalType::Alias(alias)) if alias.arity == ty.targs.len() => {
                LocalTypeTemplateTarget::Alias(alias.template)
            }
            Some(CompactLexicalType::Classifier(declaration)) => {
                LocalTypeTemplateTarget::Classifier { declaration, scope }
            }
            Some(CompactLexicalType::Alias(_)) | None => {
                let mut target = ty.clone();
                target.set_nullable(false);
                target.set_definitely_non_null(false);
                target.set_projection(false, false);
                let target_children = if ty.is_function_type_syntax() {
                    target
                        .fun_params
                        .iter_mut()
                        .chain(target.arg.iter_mut().map(Box::as_mut))
                        .collect::<Vec<_>>()
                } else {
                    target
                        .targs
                        .iter_mut()
                        .chain(target.arg.iter_mut().map(Box::as_mut))
                        .collect::<Vec<_>>()
                };
                let child_formals = target_children
                    .into_iter()
                    .enumerate()
                    .map(|(index, child)| {
                        let formal = format!("\0local-template-{}-{index}", ty.span.lo);
                        *child = TypeRef {
                            name: formal.clone(),
                            flags: TrFlags::default(),
                            arg: None,
                            targs: Vec::new(),
                            span: child.span,
                            fun_params: Box::new([]),
                            fun_context_count: 0,
                            qualifier_arguments: Default::default(),
                        };
                        self.graph.intern_name(&formal)
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                let syntax = self.graph.add_type_syntax(&target);
                LocalTypeTemplateTarget::Syntax {
                    syntax,
                    scope,
                    child_formals,
                }
            }
        };
        self.graph
            .add_local_type_template_node(LocalTypeTemplateNode {
                source,
                target,
                children,
                flags,
            })
    }
}
