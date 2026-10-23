//! The declaration an inline expansion copies its body from.
//!
//! A same-module inline function is a checked declaration of this compilation: its frames are
//! opened under its checked name, its copies keep its source file for a target's source maps, and
//! its call's substitutions name its module type parameters. An inline dependency function is
//! expanded from its template in this file ([`super::super::DependencyInlineTemplate`]): the
//! template states its source name and type parameter identities, its call's substitutions name
//! those parameters by ordinal, and its source is not part of this compilation.

use super::super::DependencyInlineTemplate;
use crate::fir::{
    CallableId, ExternalCallableId, FirTypeParameterRef, FirTypeSubstitution, TypeParameterId,
};
use crate::fx_hash::HashMap;
use crate::ir::{ExprId, FunId, IrFile};
use crate::types::{Ty, TypeName};

use crate::fir::ResolvedModuleIndex;

pub(in super::super) enum InlineCallee {
    Module(CallableId),
    Dependency {
        target: ExternalCallableId,
        template: DependencyInlineTemplate,
    },
}

/// What an expansion records about its callee.
pub(super) struct InlineFrame {
    /// The name the expansion's frames are opened under.
    pub(super) name: String,
    /// The module declaration whose body is copied. A call-site specialization of a lambda or an
    /// anonymous class the body declares is named after it; a dependency template declares none.
    pub(super) module: Option<CallableId>,
    /// The copied declaration's owner and source file, for a same-module callee.
    source: Option<(Option<TypeName>, crate::fir::SourceFileId)>,
    label: String,
}

impl InlineCallee {
    pub(super) fn frame(&self, index: &ResolvedModuleIndex) -> Option<InlineFrame> {
        Some(match self {
            Self::Module(target) => {
                let callable = index.callable(*target)?;
                let owner = index
                    .enclosing_classifier(callable.declaration)
                    .map(|classifier| classifier.classifier);
                InlineFrame {
                    name: index.callable_name(*target)?.to_owned(),
                    module: Some(*target),
                    source: Some((
                        owner,
                        index.declaration_anchor(callable.declaration)?.source,
                    )),
                    label: format!("$fir_inline${}", target.raw()),
                }
            }
            Self::Dependency { target, template } => InlineFrame {
                name: template.name.clone(),
                module: None,
                source: None,
                label: format!("$fir_dependency_inline${}", target.raw()),
            },
        })
    }

    /// The semantic bindings of the call's recorded substitutions, by the template's type
    /// parameter identities, and the reified ones' run-time types. A substitution of a parameter
    /// the callee does not declare declines.
    ///
    /// Declaration headers are intentionally not accepted here. Whether a substitution may drive a
    /// reified operation was decided while checking the call and travels on that exact
    /// substitution; common lowering only resolves the parameter to its template name.
    pub(in super::super) fn substitution_bindings<'a>(
        &'a self,
        substitutions: &[FirTypeSubstitution],
        mut module_parameter_name: impl FnMut(TypeParameterId) -> Option<&'a str>,
    ) -> Option<(HashMap<String, Ty>, HashMap<String, Ty>)> {
        let mut bindings = HashMap::default();
        let mut reified = HashMap::default();
        for substitution in substitutions {
            let name = match (self, substitution.parameter) {
                // A substitution of a parameter that has no module name binds nothing in the
                // template, as for a same-module callee it never has.
                (Self::Module(_), FirTypeParameterRef::Module(parameter)) => {
                    match module_parameter_name(parameter) {
                        Some(name) => name,
                        None => continue,
                    }
                }
                (Self::Module(_), FirTypeParameterRef::External { .. }) => continue,
                (
                    Self::Dependency { target, template },
                    FirTypeParameterRef::External { callable, ordinal },
                ) if callable == *target => {
                    template.type_parameters.get(ordinal as usize)?.as_str()
                }
                (Self::Dependency { .. }, _) => return None,
            };
            bindings.insert(name.to_owned(), substitution.value.get());
            if substitution.reified {
                reified.insert(name.to_owned(), substitution.reified_runtime.get());
            }
        }
        Some((bindings, reified))
    }
}

impl InlineFrame {
    /// Record `copy` as copied from `function`'s body.
    pub(super) fn record_copy(&self, ir: &mut IrFile, copy: ExprId, function: FunId) {
        match self.source {
            Some((owner, source)) => ir.record_inline_copy_owner(copy, function, owner, source),
            None => ir.mark_inline_copy(copy),
        }
    }

    /// The copied module declaration's owner and source file.
    pub(super) fn source(&self) -> Option<(Option<TypeName>, crate::fir::SourceFileId)> {
        self.source
    }

    /// The label of the loop an expansion's returns leave, unique by the next free slot.
    pub(super) fn return_label(&self, next_temporary: u32) -> String {
        format!("{}_{next_temporary}", self.label)
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::DependencyInlineTemplate;
    use super::InlineCallee;
    use crate::fir::{
        CallableId, ExternalCallableId, FirTypeParameterRef, FirTypeSubstitution, ResolvedTy,
        TypeParameterId,
    };
    use crate::types::Ty;

    fn substitution(
        parameter: FirTypeParameterRef,
        reified: bool,
        value: Ty,
    ) -> FirTypeSubstitution {
        FirTypeSubstitution {
            parameter,
            reified,
            value: ResolvedTy::new(value).unwrap(),
            reified_runtime: ResolvedTy::new(value).unwrap(),
            additional_bounds: Box::new([]),
        }
    }

    #[test]
    fn checked_substitution_flag_alone_selects_reified_bindings() {
        let ordinary = TypeParameterId::from_raw(3);
        let reified = TypeParameterId::from_raw(7);
        let substitutions = [
            substitution(FirTypeParameterRef::Module(ordinary), false, Ty::String),
            substitution(FirTypeParameterRef::Module(reified), true, Ty::Int),
        ];
        let callee = InlineCallee::Module(CallableId::from_raw(0));
        let (bindings, reified_bindings) = callee
            .substitution_bindings(&substitutions, |parameter| {
                if parameter == ordinary {
                    Some("T")
                } else if parameter == reified {
                    Some("R")
                } else {
                    None
                }
            })
            .expect("module substitutions bind");
        assert_eq!(
            bindings,
            [("T".into(), Ty::String), ("R".into(), Ty::Int)]
                .into_iter()
                .collect()
        );
        assert_eq!(
            reified_bindings,
            [("R".into(), Ty::Int)].into_iter().collect()
        );
    }

    fn dependency(target: ExternalCallableId) -> InlineCallee {
        InlineCallee::Dependency {
            target,
            template: DependencyInlineTemplate {
                function: 0,
                name: "scope".to_owned(),
                type_parameters: vec!["opaque-T".to_owned(), "opaque-R".to_owned()],
            },
        }
    }

    #[test]
    fn a_dependency_substitution_binds_the_template_parameter_of_its_ordinal() {
        let target = ExternalCallableId::from_raw(5);
        let substitutions = [
            substitution(
                FirTypeParameterRef::External {
                    callable: target,
                    ordinal: 1,
                },
                false,
                Ty::String,
            ),
            substitution(
                FirTypeParameterRef::External {
                    callable: target,
                    ordinal: 0,
                },
                false,
                Ty::Int,
            ),
        ];
        let callee = dependency(target);
        let (bindings, reified) = callee
            .substitution_bindings(&substitutions, |_| None)
            .expect("the callee's own substitutions bind");
        assert_eq!(
            bindings,
            [
                ("opaque-R".into(), Ty::String),
                ("opaque-T".into(), Ty::Int)
            ]
            .into_iter()
            .collect()
        );
        assert!(reified.is_empty());
    }

    #[test]
    fn a_dependency_substitution_of_another_parameter_declines() {
        let target = ExternalCallableId::from_raw(5);
        let callee = dependency(target);
        for parameter in [
            FirTypeParameterRef::External {
                callable: target,
                ordinal: 2,
            },
            FirTypeParameterRef::External {
                callable: ExternalCallableId::from_raw(6),
                ordinal: 0,
            },
            FirTypeParameterRef::Module(TypeParameterId::from_raw(0)),
        ] {
            let substitutions = [substitution(parameter, false, Ty::Int)];
            assert!(callee
                .substitution_bindings(&substitutions, |_| Some("T"))
                .is_none());
        }
    }
}
