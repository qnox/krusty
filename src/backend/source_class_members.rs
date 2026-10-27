//! The declared superclass, type parameters and instance functions of every named source
//! classifier of the module, frozen before Pass 2 starts.
//!
//! Only the declaring file's IR holds a class's members, so a target asking what a class inherits
//! from a source supertype declared in another file of the module cannot find it there. These
//! records come from the stable declaration headers every file sees alike, so the answer does not
//! depend on which file is active. A body-local classifier has no record: only its own file can
//! name it, and that file's IR declares it. Every type here is a declaration type; a target derives
//! its physical representation itself.

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
}

/// One instance function a source classifier declares.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceMemberFunction {
    pub name: Box<str>,
    pub visibility: Visibility,
    /// Declared `abstract`: the declaration has no body.
    pub is_abstract: bool,
    pub is_suspend: bool,
    /// The checked declaration shape, `None` when its header did not complete. A consumer that
    /// needs it must refuse, not skip it.
    pub signature: Option<SourceFunctionSignature>,
}

/// A source function's own type parameters and declared types.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceFunctionSignature {
    /// Semantic names of the function's own type parameters, in order: the names its types use.
    pub type_parameters: Box<[Box<str>]>,
    pub extension_receiver: Option<Ty>,
    /// Context parameters followed by the value parameters.
    pub parameters: Box<[Ty]>,
    pub result: Ty,
}

impl SourceFunctionSignature {
    /// The receiver, if any, then the context and value parameters: every declared parameter a
    /// call passes.
    pub fn receiver_and_parameters(&self) -> Vec<Ty> {
        self.extension_receiver
            .iter()
            .chain(self.parameters.iter())
            .copied()
            .collect()
    }
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
            .then_some((member, header.visibility, header.flags))
        })
        .collect::<Vec<_>>();
    owned.sort_by_key(|(member, ..)| index.source_order(*member));
    let functions = owned
        .into_iter()
        .filter_map(|(member, visibility, flags)| {
            Some(SourceMemberFunction {
                name: index.declaration_name(member)?.into(),
                visibility,
                is_abstract: flags.has(DeclarationFlags::ABSTRACT),
                is_suspend: flags.has(DeclarationFlags::SUSPEND),
                signature: signature(index, member),
            })
        })
        .collect();
    SourceClassMembers {
        superclass: superclass.filter(|&superclass| superclass != crate::types::wk::any()),
        type_parameters,
        functions,
    }
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
        extension_receiver: callable
            .shape
            .extension_receiver
            .map(|receiver| receiver.get()),
        parameters: signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect(),
        result: signature.result.get(),
    })
}
