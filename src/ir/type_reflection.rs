//! Declaration facts used to realize reflective type descriptions.

use super::{IrFile, IrGenericSig, IrModuleSource, IrTypeParameter};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// A top-level generic extension property: the declaration that owns the type parameters its
/// accessor bodies see.
#[derive(Clone, Debug)]
pub struct IrGenericTopLevelProperty {
    pub name: String,
    pub is_var: bool,
    pub getter: u32,
    pub setter: Option<u32>,
    pub type_params: Vec<IrTypeParameter>,
}

#[derive(Default)]
pub(super) struct TypeReflectionFacts {
    top_level_generic_properties: Vec<IrGenericTopLevelProperty>,
    foreign_template_facades: HashMap<u32, TypeName>,
    foreign_template_sources: HashMap<u32, IrModuleSource>,
    /// Own type parameters of another file's classifiers whose inline members this file splices:
    /// a spliced body can describe them (`typeOf<List<T>>()`) although no class here declares them.
    foreign_template_classifiers: HashMap<TypeName, Vec<IrTypeParameter>>,
    /// A suspend lambda's function → the declarations of the type parameters its function type
    /// names, directly or through their bounds, in first-use order. Reflection reads the lambda's
    /// function from its class, which records those it names.
    lambda_type_parameters: HashMap<u32, Vec<IrTypeParameter>>,
}

impl IrFile {
    pub(crate) fn record_top_level_generic_property(
        &mut self,
        property: IrGenericTopLevelProperty,
    ) {
        self.type_reflection
            .top_level_generic_properties
            .push(property);
    }

    pub(crate) fn top_level_generic_properties(&self) -> &[IrGenericTopLevelProperty] {
        &self.type_reflection.top_level_generic_properties
    }

    pub(crate) fn record_foreign_template_source(&mut self, function: u32, source: IrModuleSource) {
        self.type_reflection
            .foreign_template_sources
            .insert(function, source);
    }

    pub(crate) fn foreign_template_sources(
        &self,
    ) -> impl Iterator<Item = (u32, IrModuleSource)> + '_ {
        self.type_reflection
            .foreign_template_sources
            .iter()
            .map(|(function, source)| (*function, *source))
    }

    pub(crate) fn record_foreign_template_facades(
        &mut self,
        facades: impl IntoIterator<Item = (u32, TypeName)>,
    ) {
        self.type_reflection
            .foreign_template_facades
            .extend(facades);
    }

    pub(crate) fn foreign_template_facade(&self, function: u32) -> Option<TypeName> {
        self.type_reflection
            .foreign_template_facades
            .get(&function)
            .copied()
    }

    pub(crate) fn record_foreign_template_classifier(
        &mut self,
        classifier: TypeName,
        type_params: Vec<IrTypeParameter>,
    ) {
        self.type_reflection
            .foreign_template_classifiers
            .insert(classifier, type_params);
    }

    pub(crate) fn foreign_template_classifiers(
        &self,
    ) -> impl Iterator<Item = (TypeName, &[IrTypeParameter])> + '_ {
        self.type_reflection
            .foreign_template_classifiers
            .iter()
            .map(|(classifier, type_params)| (*classifier, type_params.as_slice()))
    }

    pub(crate) fn record_lambda_type_parameters(
        &mut self,
        lambda: u32,
        type_params: Vec<IrTypeParameter>,
    ) {
        let previous = self
            .type_reflection
            .lambda_type_parameters
            .insert(lambda, type_params);
        assert!(
            previous.is_none(),
            "a lambda's type parameters are recorded once"
        );
    }

    /// The type parameters `lambda`'s function type names, directly or through their bounds.
    pub(crate) fn lambda_type_parameters(&self, lambda: u32) -> &[IrTypeParameter] {
        self.type_reflection
            .lambda_type_parameters
            .get(&lambda)
            .expect("a suspend lambda records the type parameters its function type names")
    }

    pub fn class_signatures(&self) -> impl Iterator<Item = (TypeName, &IrGenericSig)> + '_ {
        self.class_signatures
            .iter()
            .map(|(classifier, signature)| (*classifier, signature))
    }
}

/// Each type parameter `ty` names, in the order Kotlin metadata writes the type's parts: a
/// classifier's arguments in order, and a function type as its `FunctionN` arguments (its context
/// and receiver types, its value parameters, then its result). A parameter's bound is its
/// declaration's, not a part of `ty`.
pub(crate) fn type_parameters_named_by(ty: Ty, names: &mut Vec<&'static str>) {
    match ty {
        Ty::TyParam(name, _) => {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        Ty::Obj(_, arguments) => {
            for &argument in arguments {
                type_parameters_named_by(argument, names);
            }
        }
        Ty::Fun(signature) => {
            for &parameter in &signature.params {
                type_parameters_named_by(parameter, names);
            }
            type_parameters_named_by(signature.ret, names);
        }
        Ty::DefinitelyNotNull(inner)
        | Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => type_parameters_named_by(*inner, names),
        // A star names no type; the bound it keeps is the parameter's.
        Ty::StarProjection(_) => {}
        Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => {}
    }
}
