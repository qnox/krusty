//! Structural projection between parser-expanded compact header types and their written syntax.

use super::*;

impl HeaderSyntaxArena {
    /// Add a graph-owned semantic template together with its complete written tree.
    ///
    /// Unlike [`HeaderSyntaxArena::capture_source_spellings`], this relationship is already
    /// structural: it must not rediscover descendants by source span. Function-type children may
    /// share their enclosing type's span, and treating that equality as an alias-expansion
    /// sidecar would attach the root below itself indefinitely.
    pub(in crate::fir) fn add_type_with_source_spelling(
        &mut self,
        expanded: &TypeRef,
        written: &TypeRef,
        names: &mut LookupNames,
    ) -> HeaderTypeId {
        let expanded = self.add_type(expanded, names);
        let written = self.add_type(written, names);
        self.source_spellings.insert(expanded, written);
        expanded
    }

    fn transient_type_ref_selected(
        &self,
        id: HeaderTypeId,
        names: &LookupNames,
        written: bool,
    ) -> Option<TypeRef> {
        let id = if written {
            self.source_spellings.get(&id).copied().unwrap_or(id)
        } else {
            id
        };
        let ty = self.ty(id)?;
        let flags = TrFlags::default()
            .with_nullable(ty.flags.nullable())
            .with_definitely_non_null(ty.flags.definitely_non_null())
            .with_fun_has_receiver(ty.flags.function_receiver())
            .with_fun_suspend(ty.flags.suspend_function())
            .with_in_projection(ty.flags.in_projection())
            .with_out_projection(ty.flags.out_projection())
            .with_import(ty.flags.is_import())
            .with_star_projection(ty.flags.star_projection())
            .with_class_literal_lhs(ty.flags.class_literal_lhs());
        let (name, arg, targs, fun_params, fun_context_count) = match ty.kind {
            HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            } => {
                let detail = self.classifier_type(detail)?;
                let name = self
                    .type_path(detail.path)
                    .iter()
                    .map(|segment| names.get(*segment))
                    .collect::<Option<Vec<_>>>()?
                    .join(".");
                let mut targs = Vec::new();
                for argument in self.type_operands(detail.arguments) {
                    targs.push(self.transient_type_ref_selected(*argument, names, written)?);
                }
                let arg = match abbreviated_argument {
                    Some(argument) => Some(Box::new(
                        self.transient_type_ref_selected(argument, names, written)?,
                    )),
                    None => None,
                };
                (name, arg, targs, Vec::new().into_boxed_slice(), 0)
            }
            HeaderTypeKind::Function {
                parameters,
                result,
                context_count,
            } => {
                let mut fun_params = Vec::new();
                for parameter in self.type_operands(parameters) {
                    fun_params.push(self.transient_type_ref_selected(*parameter, names, written)?);
                }
                let arg = match result {
                    Some(result) => Some(Box::new(
                        self.transient_type_ref_selected(result, names, written)?,
                    )),
                    None => None,
                };
                (
                    "<fun>".into(),
                    arg,
                    Vec::new(),
                    fun_params.into_boxed_slice(),
                    context_count,
                )
            }
        };
        Some(TypeRef {
            name,
            flags,
            arg,
            targs,
            span: ty.span,
            fun_params,
            fun_context_count,
            qualifier_arguments: self
                .qualifier_arguments
                .get(&id)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub fn transient_type_ref(&self, id: HeaderTypeId, names: &LookupNames) -> Option<TypeRef> {
        self.transient_type_ref_selected(id, names, false)
    }

    pub(crate) fn transient_written_type_ref(
        &self,
        id: HeaderTypeId,
        names: &LookupNames,
    ) -> Option<TypeRef> {
        self.transient_type_ref_selected(id, names, true)
    }

    pub(crate) fn written_type_id(&self, id: HeaderTypeId) -> HeaderTypeId {
        self.source_spellings.get(&id).copied().unwrap_or(id)
    }

    /// Whether this compact type tree contains a parser-recorded source-spelling projection.
    ///
    /// The relationship is queried by compact identity, before either tree is rendered. A nested
    /// alias rewrite therefore remains visible even when the expanded and written classifiers
    /// happen to share a source span or source name.
    pub(crate) fn has_source_spelling_projection(&self, root: HeaderTypeId) -> bool {
        fn visit(arena: &HeaderSyntaxArena, id: HeaderTypeId) -> bool {
            if arena.source_spellings.contains_key(&id) {
                return true;
            }
            let Some(ty) = arena.ty(id) else {
                return false;
            };
            match ty.kind {
                HeaderTypeKind::Classifier {
                    detail,
                    abbreviated_argument,
                } => {
                    arena.classifier_type(detail).is_some_and(|detail| {
                        arena
                            .type_operands(detail.arguments)
                            .iter()
                            .copied()
                            .any(|argument| visit(arena, argument))
                    }) || abbreviated_argument.is_some_and(|argument| visit(arena, argument))
                }
                HeaderTypeKind::Function {
                    parameters, result, ..
                } => {
                    arena
                        .type_operands(parameters)
                        .iter()
                        .copied()
                        .any(|parameter| visit(arena, parameter))
                        || result.is_some_and(|result| visit(arena, result))
                }
            }
        }

        visit(self, root)
    }

    pub(crate) fn written_type_node(
        &self,
        id: HeaderTypeId,
    ) -> Option<(HeaderTypeId, bool, Vec<HeaderTypeId>)> {
        let id = self.written_type_id(id);
        let ty = self.ty(id)?;
        let (function, children) = match ty.kind {
            HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            } => {
                let detail = self.classifier_type(detail)?;
                (
                    false,
                    self.type_operands(detail.arguments)
                        .iter()
                        .copied()
                        .chain(abbreviated_argument)
                        .collect(),
                )
            }
            HeaderTypeKind::Function {
                parameters, result, ..
            } => (
                true,
                self.type_operands(parameters)
                    .iter()
                    .copied()
                    .chain(result)
                    .collect(),
            ),
        };
        Some((id, function, children))
    }

    pub(crate) fn resolved_written_spelling_preorder(
        &self,
        root: HeaderTypeId,
        classifier_segment: &mut dyn FnMut(HeaderTypeId) -> Option<Option<u32>>,
    ) -> Option<Vec<HeaderTypeId>> {
        fn visit(
            arena: &HeaderSyntaxArena,
            id: HeaderTypeId,
            classifier_segment: &mut dyn FnMut(HeaderTypeId) -> Option<Option<u32>>,
            out: &mut Vec<HeaderTypeId>,
        ) -> Option<()> {
            let id = arena.written_type_id(id);
            let ty = arena.ty(id)?;
            let selected_segment = classifier_segment(id)?;
            out.push(id);
            match ty.kind {
                HeaderTypeKind::Classifier { detail, .. } => {
                    let detail = arena.classifier_type(detail)?;
                    let arguments = arena.type_operands(detail.arguments);
                    let stripped = selected_segment
                        .map(|segment| {
                            arena
                                .qualifier_arguments
                                .get(&id)
                                .into_iter()
                                .flatten()
                                .filter(|arguments| arguments.segment < segment)
                                .map(|arguments| arguments.arguments.len())
                                .sum::<usize>()
                        })
                        .unwrap_or(0);
                    let kept = arguments.len().saturating_sub(stripped);
                    for argument in &arguments[..kept] {
                        visit(arena, *argument, classifier_segment, out)?;
                    }
                }
                HeaderTypeKind::Function {
                    parameters, result, ..
                } => {
                    for parameter in arena.type_operands(parameters) {
                        visit(arena, *parameter, classifier_segment, out)?;
                    }
                    if !ty.flags.suspend_function() {
                        if let Some(result) = result {
                            visit(arena, result, classifier_segment, out)?;
                        }
                    }
                }
            }
            Some(())
        }

        let mut ids = Vec::new();
        visit(self, root, classifier_segment, &mut ids)?;
        Some(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(name: &str, span: crate::diag::Span) -> TypeRef {
        TypeRef {
            name: name.to_owned(),
            flags: TrFlags::default(),
            arg: None,
            targs: Vec::new(),
            span,
            fun_params: Box::new([]),
            fun_context_count: 0,
            qualifier_arguments: Default::default(),
        }
    }

    #[test]
    fn an_explicit_written_function_tree_does_not_rediscover_its_root_by_span() {
        let shared = crate::diag::Span::new(7, 13);
        let result = crate::diag::Span::new(20, 24);
        let function = |receiver| TypeRef {
            name: "<fun>".to_owned(),
            flags: TrFlags::default().with_fun_has_receiver(true),
            arg: Some(Box::new(leaf("Unit", result))),
            targs: Vec::new(),
            span: shared,
            fun_params: vec![receiver].into_boxed_slice(),
            fun_context_count: 0,
            qualifier_arguments: Default::default(),
        };
        let expanded = function(leaf("\0formal", shared));
        let written = function(leaf("String", shared));
        let mut arena = HeaderSyntaxArena::default();
        let mut names = LookupNames::default();

        let root = arena.add_type_with_source_spelling(&expanded, &written, &mut names);
        let projected = arena
            .transient_written_type_ref(root, &names)
            .expect("the explicit written tree must remain finite and materializable");

        assert!(projected.fun_has_receiver());
        assert_eq!(projected.fun_params.len(), 1);
        assert_eq!(projected.fun_params[0].name, "String");
        assert_eq!(projected.fun_params[0].span, shared);
        assert_eq!(
            projected.arg.as_deref().map(|result| result.name.as_str()),
            Some("Unit")
        );
        assert!(arena.has_source_spelling_projection(root));
    }

    #[test]
    fn a_nested_source_spelling_projection_is_found_by_compact_identity() {
        let span = crate::diag::Span::new(7, 13);
        let mut arena = HeaderSyntaxArena::default();
        let mut names = LookupNames::default();
        let root = arena.add_type(
            &TypeRef {
                name: "List".to_owned(),
                flags: TrFlags::default(),
                arg: None,
                targs: vec![leaf("String", span)],
                span,
                fun_params: Box::new([]),
                fun_context_count: 0,
                qualifier_arguments: Default::default(),
            },
            &mut names,
        );
        let child = match arena.ty(root).expect("root").kind {
            HeaderTypeKind::Classifier { detail, .. } => {
                let detail = arena.classifier_type(detail).expect("root detail");
                arena.type_operands(detail.arguments)[0]
            }
            HeaderTypeKind::Function { .. } => panic!("classifier root"),
        };
        let written = arena.add_type(&leaf("Text", span), &mut names);
        arena.source_spellings.insert(child, written);

        assert!(arena.has_source_spelling_projection(root));
    }
}
