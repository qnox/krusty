//! The declared superclass and instance functions of every named source classifier of the module,
//! frozen before Pass 2 starts.
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
    /// Instance functions in source order; a companion's members belong to the companion.
    pub functions: Box<[SourceMemberFunction]>,
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
        extension: callable.shape.extension_receiver.is_some(),
        parameters: signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect(),
        result: signature.result.get(),
    })
}
