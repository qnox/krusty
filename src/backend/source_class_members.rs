//! The declared superclass, type parameters, instance functions and instance properties of every
//! named source classifier of the module, frozen before Pass 2 starts.
//!
//! Only the declaring file's IR holds a class's members, so a target asking what a class inherits
//! from a source supertype declared in another file of the module cannot find it there. These
//! records come from the stable declaration headers every file sees alike, so the answer does not
//! depend on which file is active. A body-local classifier has no record: only its own file can
//! name it, and that file's IR declares it.

use crate::fir::{DeclarationFlags, DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::types::{Ty, TypeName, Visibility};

/// What a named source classifier declares for its subclasses to inherit.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceClassMembers {
    /// The declared superclass, `None` for an interface or a class extending `kotlin.Any`.
    pub superclass: Option<TypeName>,
    /// Semantic names of every type parameter an application of the classifier binds, in order:
    /// its own, then those it captures from enclosing declarations. `None` when a header did not
    /// complete (a module that recovered from a frontend error).
    pub type_parameters: Option<Box<[Box<str>]>>,
    /// Instance functions in source order; a companion's members belong to the companion.
    pub functions: Box<[SourceMemberFunction]>,
    /// Instance properties in source order.
    pub properties: Box<[SourceMemberProperty]>,
}

/// One instance property a source classifier declares.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceMemberProperty {
    pub name: Box<str>,
    pub visibility: Visibility,
    /// What its accessors take besides the dispatch receiver, `None` when its header did not
    /// complete. A consumer that needs it must refuse, not skip it.
    pub receivers: Option<SourcePropertyReceivers>,
}

/// The receivers and context parameters a source property's accessors take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourcePropertyReceivers {
    pub extension: bool,
    pub context_parameters: u32,
}

/// One instance function a source classifier declares.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceMemberFunction {
    pub name: Box<str>,
    pub visibility: Visibility,
    /// The checked declaration shape, `None` when its header did not complete (a module that
    /// recovered from a frontend error). A consumer that needs it must refuse, not skip it.
    pub signature: Option<SourceFunctionSignature>,
}

/// A source function's own type parameters and declared types.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceFunctionSignature {
    /// Semantic names of the function's own type parameters, in order: the names its types use.
    pub type_parameters: Box<[Box<str>]>,
    pub extension: bool,
    /// Context parameters followed by the value parameters.
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

pub(super) fn from_resolved_index(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    superclass: Option<TypeName>,
) -> SourceClassMembers {
    let type_parameters = index
        .classifier_type_arguments(declaration)
        .unwrap_or_default()
        .iter()
        .map(|&parameter| index.type_parameter_semantic_name(parameter).map(Box::from))
        .collect::<Option<Box<[_]>>>();
    let mut owned = index
        .owned_declarations(declaration)
        .iter()
        .copied()
        .filter_map(|member| {
            let header = index.declaration_header(member)?;
            (header.kind == DeclarationKind::Function
                && !header.flags.has(DeclarationFlags::COMPANION)
                && !index.is_suppressed_generated_callable(member))
            .then_some((member, header.visibility))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|(member, _)| index.source_order(*member));
    let functions = owned
        .into_iter()
        .filter_map(|(member, visibility)| {
            Some(SourceMemberFunction {
                name: index.declaration_name(member)?.into(),
                visibility,
                signature: signature(index, member),
            })
        })
        .collect();
    SourceClassMembers {
        superclass: superclass.filter(|&superclass| superclass != crate::types::wk::any()),
        type_parameters,
        functions,
        properties: properties(index, declaration),
    }
}

fn properties(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Box<[SourceMemberProperty]> {
    let mut owned = index
        .owned_declarations(declaration)
        .iter()
        .copied()
        .filter_map(|member| {
            let header = index.declaration_header(member)?;
            (header.kind == DeclarationKind::Property
                && !header.flags.has(DeclarationFlags::COMPANION))
            .then_some((member, header.visibility))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|(member, _)| index.source_order(*member));
    owned
        .into_iter()
        .filter_map(|(member, visibility)| {
            let receivers = index
                .property_for_declaration(member)
                .and_then(|property| index.property(property))
                .map(|header| SourcePropertyReceivers {
                    extension: header.extension_receiver.is_some(),
                    context_parameters: header.context_parameter_count,
                });
            Some(SourceMemberProperty {
                name: index.declaration_name(member)?.into(),
                visibility,
                receivers,
            })
        })
        .collect()
}

fn signature(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Option<SourceFunctionSignature> {
    let callable = index.callable_for_declaration(declaration)?;
    let signature = index.signature(declaration)?;
    let type_parameters = (0..)
        .map_while(|ordinal| index.type_parameter(declaration, ordinal))
        .map(|parameter| index.type_parameter_semantic_name(parameter).map(Box::from))
        .collect::<Option<Box<[_]>>>()?;
    Some(SourceFunctionSignature {
        type_parameters,
        extension: callable.shape.extension_receiver.is_some(),
        parameters: signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect(),
        result: signature.result.get(),
    })
}

/// Checked annotation payloads of registered module member declarations, before any file emits.
/// An empty value proves a known unannotated declaration; a missing identity proves nothing.
#[derive(Clone, Debug, Default)]
pub struct SourceMemberAnnotations {
    functions: crate::fx_hash::HashMap<crate::fir::CallableId, crate::ir::DeclarationAnnotations>,
    getters: crate::fx_hash::HashMap<crate::fir::PropertyId, crate::ir::DeclarationAnnotations>,
    setters: crate::fx_hash::HashMap<crate::fir::PropertyId, crate::ir::DeclarationAnnotations>,
}

impl SourceMemberAnnotations {
    pub(super) fn record_class(
        &mut self,
        index: &ResolvedModuleIndex,
        class: DeclarationId,
    ) -> Result<(), super::BackendFactError> {
        for &declaration in index.owned_declarations(class) {
            let Some(header) = index.declaration_header(declaration) else {
                continue;
            };
            match header.kind {
                DeclarationKind::Function => {
                    let Some(callable) = index.callable_for_declaration(declaration) else {
                        continue;
                    };
                    let annotations = checked_applications(
                        index,
                        index.declaration_applied_annotations(declaration),
                    )?;
                    assert!(
                        self.functions.insert(callable.id, annotations).is_none(),
                        "a source member declaration is frozen once"
                    );
                }
                DeclarationKind::Property => {
                    let Some(property) = index.property_for_declaration(declaration) else {
                        continue;
                    };
                    let getter = checked_applications(
                        index,
                        index.property_accessor_applied_annotations(
                            declaration,
                            crate::fir::PropertyAccessor::Getter,
                        ),
                    )?;
                    assert!(
                        self.getters.insert(property, getter).is_none(),
                        "a source getter declaration is frozen once"
                    );
                    if index
                        .property(property)
                        .expect("a published property keeps its header")
                        .mutable
                    {
                        let setter = checked_applications(
                            index,
                            index.property_accessor_applied_annotations(
                                declaration,
                                crate::fir::PropertyAccessor::Setter,
                            ),
                        )?;
                        assert!(
                            self.setters.insert(property, setter).is_none(),
                            "a source setter declaration is frozen once"
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn function(
        &self,
        callable: crate::fir::CallableId,
    ) -> Option<&crate::ir::DeclarationAnnotations> {
        self.functions.get(&callable)
    }

    pub fn getter(
        &self,
        property: crate::fir::PropertyId,
    ) -> Option<&crate::ir::DeclarationAnnotations> {
        self.getters.get(&property)
    }

    pub fn setter(
        &self,
        property: crate::fir::PropertyId,
    ) -> Option<&crate::ir::DeclarationAnnotations> {
        self.setters.get(&property)
    }
}

fn checked_applications(
    index: &ResolvedModuleIndex,
    applications: &[crate::types::ResolvedAnnotation],
) -> Result<crate::ir::DeclarationAnnotations, super::BackendFactError> {
    let applications = applications
        .iter()
        .map(|application| {
            let retention = index
                .checked_annotation_retention(application.annotation)
                .ok_or(super::BackendFactError::IncompleteAnnotation(
                    application.annotation,
                ))?;
            Ok((application, retention))
        })
        .collect::<Result<Vec<_>, super::BackendFactError>>()?;
    Ok(crate::ir::DeclarationAnnotations::from_resolved(
        applications,
    ))
}

#[cfg(test)]
mod annotation_tests;
