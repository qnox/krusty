//! Structural projection between parser-expanded compact header types and their written syntax.

use super::*;

impl HeaderSyntaxArena {
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
                (name, arg, targs, Vec::new(), 0)
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
                ("<fun>".into(), arg, Vec::new(), fun_params, context_count)
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
                .map(|lists| lists.to_vec())
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
                                .map(|arguments| arguments.count as usize)
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
