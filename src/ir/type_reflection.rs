//! Declaration facts used to realize reflective type descriptions.

use super::{IrExpr, IrFile, IrGenericSig, IrModuleSource, IrTypeParameter};
use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// Whether a lambda class is a source Kotlin function or a compiler-synthesized adapter.
///
/// Class-strategy reflection writes `@Metadata` only for a source function. A missing function
/// type or parameter list is not evidence that the class is an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LambdaClassProvenance {
    SourceFunction,
    SynthesizedAdapter,
}

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
    /// A source lambda's function → the declarations of the type parameters its function type
    /// names, directly or through their bounds, in first-use order. The checker published those
    /// identities; reflection reads them from the lambda class.
    lambda_type_parameters: HashMap<u32, Vec<IrTypeParameter>>,
    /// Lambda implementation → whether its class is a source function or a synthesized adapter.
    lambda_class_provenance: HashMap<u32, LambdaClassProvenance>,
}

impl IrFile {
    pub(super) fn remap_type_reflection_classifier_identities(
        &mut self,
        names: &HashMap<TypeName, TypeName>,
        mut remap_ty: impl FnMut(Ty) -> Ty,
    ) {
        let remap_parameters = |parameters: &mut [IrTypeParameter],
                                remap_ty: &mut dyn FnMut(Ty) -> Ty| {
            for parameter in parameters {
                for (bound, _) in &mut parameter.bounds {
                    *bound = remap_ty(*bound);
                }
            }
        };

        for property in &mut self.type_reflection.top_level_generic_properties {
            remap_parameters(&mut property.type_params, &mut remap_ty);
        }
        for facade in self.type_reflection.foreign_template_facades.values_mut() {
            *facade = names.get(facade).copied().unwrap_or(*facade);
        }
        for source in self.type_reflection.foreign_template_sources.values_mut() {
            source.package = names
                .get(&source.package)
                .copied()
                .unwrap_or(source.package);
        }
        for parameters in self
            .type_reflection
            .foreign_template_classifiers
            .values_mut()
        {
            remap_parameters(parameters, &mut remap_ty);
        }
        self.type_reflection.foreign_template_classifiers =
            std::mem::take(&mut self.type_reflection.foreign_template_classifiers)
                .into_iter()
                .map(|(classifier, parameters)| {
                    (
                        names.get(&classifier).copied().unwrap_or(classifier),
                        parameters,
                    )
                })
                .collect();
        for parameters in self.type_reflection.lambda_type_parameters.values_mut() {
            remap_parameters(parameters, &mut remap_ty);
        }
    }

    pub(super) fn remap_lambda_type_parameter_classifiers(
        &mut self,
        lambda: u32,
        mut remap_ty: impl FnMut(Ty) -> Ty,
    ) {
        let Some(parameters) = self.type_reflection.lambda_type_parameters.get_mut(&lambda) else {
            return;
        };
        for parameter in parameters {
            for (bound, _) in &mut parameter.bounds {
                *bound = remap_ty(*bound);
            }
        }
    }

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

    pub(crate) fn record_lambda_class_provenance(
        &mut self,
        lambda: u32,
        provenance: LambdaClassProvenance,
    ) {
        if let Some(previous) = self
            .type_reflection
            .lambda_class_provenance
            .insert(lambda, provenance)
        {
            assert_eq!(
                previous, provenance,
                "a lambda's class provenance is recorded once"
            );
        }
    }

    pub(crate) fn copy_lambda_class_provenance(&mut self, source: u32, target: u32) {
        let Some(provenance) = self
            .type_reflection
            .lambda_class_provenance
            .get(&source)
            .copied()
        else {
            return;
        };
        self.record_lambda_class_provenance(target, provenance);
    }

    pub(crate) fn lambda_class_provenance(&self, lambda: u32) -> Option<LambdaClassProvenance> {
        self.type_reflection
            .lambda_class_provenance
            .get(&lambda)
            .copied()
    }

    /// The expression is a synthesized function adapter. Its class omits the source-lambda record.
    pub(crate) fn note_synthesized_lambda(&mut self, expression: u32) {
        let IrExpr::Lambda { impl_fn, .. } = self.exprs[expression as usize] else {
            panic!("a synthesized adapter is a lambda expression");
        };
        self.record_lambda_class_provenance(impl_fn, LambdaClassProvenance::SynthesizedAdapter);
    }

    pub(crate) fn copy_lambda_type_parameters(
        &mut self,
        source: u32,
        target: u32,
        bindings: &HashMap<String, Ty>,
    ) {
        let Some(mut parameters) = self
            .type_reflection
            .lambda_type_parameters
            .get(&source)
            .cloned()
        else {
            return;
        };
        for parameter in &mut parameters {
            for (bound, _) in &mut parameter.bounds {
                *bound = crate::types::ty_subst_keep_unbound(*bound, bindings);
            }
        }
        self.record_lambda_type_parameters(target, parameters);
    }

    /// The type parameters `lambda`'s function type names, directly or through their bounds.
    pub(crate) fn lambda_type_parameters(&self, lambda: u32) -> &[IrTypeParameter] {
        self.recorded_lambda_type_parameters(lambda)
            .expect("a lambda records the type parameters its function type names")
    }

    /// The type parameters recorded for `lambda`, when lowering published them.
    pub(crate) fn recorded_lambda_type_parameters(
        &self,
        lambda: u32,
    ) -> Option<&[IrTypeParameter]> {
        self.type_reflection
            .lambda_type_parameters
            .get(&lambda)
            .map(Vec::as_slice)
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
        Ty::Intersection(parts) => {
            for &part in parts {
                type_parameters_named_by(part, names);
            }
        }
        // A star names no type; the bound it keeps is the parameter's.
        Ty::StarProjection(_) => {}
        Ty::Unit | Ty::Null | Ty::Nothing | Ty::Error | Ty::Pending => {}
    }
}
