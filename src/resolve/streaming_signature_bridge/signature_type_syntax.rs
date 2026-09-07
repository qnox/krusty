//! One read-only view over parser and compact declaration type syntax.
//!
//! Signature lookup is shared while migration callers still carry parser `TypeRef`s. Compact Pass
//! 1 callers use `HeaderTypeId` directly, so they never reconstruct an AST-shaped type tree.

use std::borrow::Cow;

use crate::ast::TypeRef;
use crate::diag::Span;
use crate::fir::{HeaderSyntaxArena, HeaderTypeId, HeaderTypeKind, LookupNames};
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy)]
pub(in crate::resolve) enum SignatureTypeSyntax<'a> {
    Parser(&'a TypeRef),
    Compact {
        arena: &'a HeaderSyntaxArena,
        names: &'a LookupNames,
        id: HeaderTypeId,
    },
}

impl<'a> SignatureTypeSyntax<'a> {
    pub(in crate::resolve) fn parser(reference: &'a TypeRef) -> Self {
        Self::Parser(reference)
    }

    pub(in crate::resolve) fn compact(
        arena: &'a HeaderSyntaxArena,
        names: &'a LookupNames,
        id: HeaderTypeId,
    ) -> Self {
        Self::Compact { arena, names, id }
    }

    pub(in crate::resolve) fn spelling(self) -> Option<Cow<'a, str>> {
        match self {
            Self::Parser(reference) => Some(Cow::Borrowed(&reference.name)),
            Self::Compact { arena, names, id } => arena
                .classifier_spelling(id, names)
                .map(Cow::Owned)
                .or_else(|| {
                    matches!(arena.ty(id)?.kind, HeaderTypeKind::Function { .. })
                        .then_some(Cow::Borrowed("<fun>"))
                }),
        }
    }

    pub(super) fn contextual_classifier_spelling(self) -> Option<(Cow<'a, str>, bool)> {
        match self {
            Self::Parser(reference) => (reference.targs.is_empty()
                && reference.arg.is_none()
                && reference.fun_params.is_empty()
                && !reference.name.contains(['.', '/', '$']))
            .then(|| (Cow::Borrowed(reference.name.as_str()), reference.nullable())),
            Self::Compact { arena, names, id } => {
                let ty = arena.ty(id)?;
                let HeaderTypeKind::Classifier {
                    detail,
                    abbreviated_argument: None,
                } = ty.kind
                else {
                    return None;
                };
                let detail = arena.classifier_type(detail)?;
                if !arena.type_operands(detail.arguments).is_empty() {
                    return None;
                }
                let [name] = arena.type_path(detail.path) else {
                    return None;
                };
                let name = names.get(*name)?;
                (!name.contains(['.', '/', '$']))
                    .then(|| (Cow::Borrowed(name), ty.flags.nullable()))
            }
        }
    }

    pub(super) fn span(self) -> Option<Span> {
        match self {
            Self::Parser(reference) => Some(reference.span),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.span),
        }
    }

    pub(super) fn nullable(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.nullable()),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.flags.nullable()),
        }
    }

    pub(super) fn is_import(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.is_import()),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.flags.is_import()),
        }
    }

    pub(super) fn definitely_non_null(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.definitely_non_null()),
            Self::Compact { arena, id, .. } => {
                arena.ty(id).map(|ty| ty.flags.definitely_non_null())
            }
        }
    }

    pub(super) fn in_projection(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.in_projection()),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.flags.in_projection()),
        }
    }

    pub(super) fn out_projection(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.out_projection()),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.flags.out_projection()),
        }
    }

    pub(super) fn star_projection(self) -> Option<bool> {
        match self {
            Self::Parser(reference) => Some(reference.is_star_projection()),
            Self::Compact { arena, id, .. } => arena.ty(id).map(|ty| ty.flags.star_projection()),
        }
    }

    pub(super) fn projected(self, resolved: Ty, star_upper_bound: Ty) -> Option<Ty> {
        Some(if self.star_projection()? {
            Ty::star_projection(star_upper_bound)
        } else if self.in_projection()? {
            Ty::in_projection(resolved)
        } else if self.out_projection()? {
            Ty::out_projection(resolved)
        } else {
            resolved
        })
    }

    pub(in crate::resolve) fn arguments(self) -> Option<Vec<Self>> {
        match self {
            Self::Parser(reference) => Some(
                reference
                    .targs
                    .iter()
                    .map(SignatureTypeSyntax::Parser)
                    .collect(),
            ),
            Self::Compact { arena, names, id } => {
                let ty = arena.ty(id)?;
                let HeaderTypeKind::Classifier { detail, .. } = ty.kind else {
                    return Some(Vec::new());
                };
                let detail = arena.classifier_type(detail)?;
                Some(
                    arena
                        .type_operands(detail.arguments)
                        .iter()
                        .copied()
                        .map(|id| Self::Compact { arena, names, id })
                        .collect(),
                )
            }
        }
    }

    pub(super) fn nested(self) -> Option<Vec<Self>> {
        match self {
            Self::Parser(reference) => Some(
                reference
                    .fun_params
                    .iter()
                    .map(SignatureTypeSyntax::Parser)
                    .chain(reference.arg.iter().map(|argument| Self::Parser(argument)))
                    .chain(reference.targs.iter().map(SignatureTypeSyntax::Parser))
                    .collect(),
            ),
            Self::Compact { arena, names, id } => {
                let ty = arena.ty(id)?;
                let children = match ty.kind {
                    HeaderTypeKind::Classifier {
                        detail,
                        abbreviated_argument,
                    } => {
                        let detail = arena.classifier_type(detail)?;
                        arena
                            .type_operands(detail.arguments)
                            .iter()
                            .copied()
                            .chain(abbreviated_argument)
                            .collect::<Vec<_>>()
                    }
                    HeaderTypeKind::Function {
                        parameters, result, ..
                    } => arena
                        .type_operands(parameters)
                        .iter()
                        .copied()
                        .chain(result)
                        .collect(),
                };
                Some(
                    children
                        .into_iter()
                        .map(|id| Self::Compact { arena, names, id })
                        .collect(),
                )
            }
        }
    }

    pub(super) fn function_shape(self) -> Option<Option<SignatureFunctionSyntax<'a>>> {
        match self {
            Self::Parser(reference) => {
                if reference.fun_params.is_empty() && reference.name != "<fun>" {
                    return Some(None);
                }
                Some(Some(SignatureFunctionSyntax {
                    parameters: reference
                        .fun_params
                        .iter()
                        .map(SignatureTypeSyntax::Parser)
                        .collect(),
                    result: reference.arg.as_deref().map(SignatureTypeSyntax::Parser),
                    context_count: usize::try_from(reference.fun_context_count).ok()?,
                    has_receiver: reference.fun_has_receiver(),
                    suspend: reference.fun_suspend(),
                }))
            }
            Self::Compact { arena, names, id } => {
                let ty = arena.ty(id)?;
                let HeaderTypeKind::Function {
                    parameters,
                    result,
                    context_count,
                } = ty.kind
                else {
                    return Some(None);
                };
                Some(Some(SignatureFunctionSyntax {
                    parameters: arena
                        .type_operands(parameters)
                        .iter()
                        .copied()
                        .map(|id| Self::Compact { arena, names, id })
                        .collect(),
                    result: result.map(|id| Self::Compact { arena, names, id }),
                    context_count: usize::try_from(context_count).ok()?,
                    has_receiver: ty.flags.function_receiver(),
                    suspend: ty.flags.suspend_function(),
                }))
            }
        }
    }

    pub(super) fn bare_parameter_spelling(self) -> Option<String> {
        if self.nullable()? || self.definitely_non_null()? || self.function_shape()?.is_some() {
            return None;
        }
        let spelling = self.spelling()?;
        (self.arguments()?.is_empty() && !spelling.contains(['.', '/', '$']))
            .then(|| spelling.into_owned())
    }

    pub(super) fn formal_occurrences(self, name: &str, projected: bool) -> Option<(bool, bool)> {
        let projected = projected || self.in_projection()? || self.out_projection()?;
        let spelling = self.spelling()?;
        let mut occurrences = (
            projected && spelling == name,
            !projected && spelling == name,
        );
        for child in self.nested()? {
            let child = child.formal_occurrences(name, projected)?;
            occurrences.0 |= child.0;
            occurrences.1 |= child.1;
        }
        Some(occurrences)
    }

    /// Resolve the restricted semantic shape used for type-parameter upper bounds. Classifier
    /// identity still comes from the declaration's ordinary class-name table; local and enclosing
    /// type parameters are supplied by the shared recursive bound builder.
    pub(super) fn tparam_bound_semantic_with(
        self,
        resolve: &dyn Fn(&str) -> Option<TypeName>,
        parameter: &mut dyn FnMut(&str) -> Option<Ty>,
    ) -> Option<Ty> {
        let nullable = self.nullable()?;
        let spelling = self.spelling()?;
        let arguments = self.arguments()?;
        let parameter_reference = (self.function_shape()?.is_none()
            && arguments.is_empty()
            && !spelling.contains(['.', '/', '$']))
        .then(|| parameter(&spelling))
        .flatten();
        let base = if let Some(parameter) = parameter_reference {
            parameter
        } else if let Some(function) = self.function_shape()? {
            let parameter_count = function.parameters.len();
            let mut parameters = Vec::with_capacity(parameter_count);
            for component in function.parameters {
                parameters.push(if component.star_projection()? {
                    Ty::nullable(Ty::obj("kotlin/Any"))
                } else {
                    component.tparam_bound_semantic_with(resolve, parameter)?
                });
            }
            let result = match function.result {
                Some(result) if result.star_projection()? => Ty::nullable(Ty::obj("kotlin/Any")),
                Some(result) => result.tparam_bound_semantic_with(resolve, parameter)?,
                None => Ty::Unit,
            };
            Ty::fun_with_shape(
                parameters,
                result,
                function.context_count.min(parameter_count),
                function.has_receiver,
                function.suspend,
            )
        } else if let Some(builtin) = Ty::from_name(&spelling) {
            builtin
        } else if let Some(element) = Ty::primitive_array_element(&spelling) {
            Ty::array(element)
        } else if let Some(classifier) = resolve(&spelling) {
            let star_bound = Ty::nullable(Ty::obj("kotlin/Any"));
            let arguments = arguments
                .into_iter()
                .map(|argument| {
                    let resolved = if argument.star_projection()? {
                        Ty::Error
                    } else {
                        argument.tparam_bound_semantic_with(resolve, parameter)?
                    };
                    argument.projected(resolved, star_bound)
                })
                .collect::<Option<Vec<_>>>()?;
            Ty::obj_args_name(classifier, &arguments)
        } else {
            Ty::obj("kotlin/Any")
        };
        Some(if nullable { Ty::nullable(base) } else { base })
    }

    /// AST materialization is restricted to diagnostic rendering for compact callers.
    pub(super) fn diagnostic_type_ref(self) -> Option<TypeRef> {
        match self {
            Self::Parser(reference) => Some(reference.clone()),
            Self::Compact { arena, names, id } => arena.diagnostic_type_ref(id, names),
        }
    }
}

pub(super) struct SignatureFunctionSyntax<'a> {
    pub(super) parameters: Vec<SignatureTypeSyntax<'a>>,
    pub(super) result: Option<SignatureTypeSyntax<'a>>,
    pub(super) context_count: usize,
    pub(super) has_receiver: bool,
    pub(super) suspend: bool,
}
