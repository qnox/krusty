use crate::fir::{DeclarationId, ResolvedClassifierHeader, ResolvedModuleIndex, TypeParameterId};
use crate::ir::{IrClass, IrFile, IrGenericSig, IrTypeParameter};
use crate::types::{wk, Ty};

pub(super) fn declaration_type_parameters(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> Vec<IrTypeParameter> {
    let mut parameters = Vec::new();
    for ordinal in 0.. {
        let Some(parameter) = index.type_parameter(declaration, ordinal) else {
            break;
        };
        let header = index
            .type_parameter_header(parameter)
            .expect("a published type-parameter identity must own compact semantic facts");
        parameters.push(IrTypeParameter {
            name: index
                .type_parameter_name(parameter)
                .expect("a type parameter must retain its declared metadata name")
                .to_owned(),
            semantic_name: index
                .type_parameter_semantic_name(parameter)
                .expect("a type parameter must retain its resolved semantic name")
                .to_owned(),
            bounds: header
                .bounds
                .iter()
                .map(|bound| (bound.ty.get(), bound.is_interface))
                .collect(),
            variance: header.flags.variance(),
            reified: header.flags.is_reified(),
        });
    }
    parameters
}

fn type_parameter(index: &ResolvedModuleIndex, parameter: TypeParameterId) -> IrTypeParameter {
    let header = index
        .type_parameter_header(parameter)
        .expect("a published type-parameter identity must own compact semantic facts");
    IrTypeParameter {
        name: index
            .type_parameter_name(parameter)
            .expect("a type parameter must retain its declared metadata name")
            .to_owned(),
        semantic_name: index
            .type_parameter_semantic_name(parameter)
            .expect("a type parameter must retain its resolved semantic name")
            .to_owned(),
        bounds: header
            .bounds
            .iter()
            .map(|bound| (bound.ty.get(), bound.is_interface))
            .collect(),
        variance: header.flags.variance(),
        reified: header.flags.is_reified(),
    }
}

pub(super) fn attach_classifier_generic_facts(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    header: &ResolvedClassifierHeader,
    class: &mut IrClass,
    ir: &mut IrFile,
) {
    let layout = index
        .classifier_type_arguments(declaration)
        .expect("a published classifier must retain its applied type-parameter layout");
    let own_count = index
        .classifier_own_type_parameter_count(declaration)
        .expect("a published classifier must distinguish own and captured type parameters")
        as usize;
    let parameters = layout
        .iter()
        .copied()
        .map(|parameter| type_parameter(index, parameter))
        .collect::<Vec<_>>();
    let (own_parameters, captured_parameters) = parameters.split_at(own_count);
    class.type_params = own_parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    class.type_param_bounds = own_parameters
        .iter()
        .flat_map(|parameter| {
            parameter
                .bounds
                .iter()
                .map(|(bound, _)| (parameter.name.clone(), *bound))
        })
        .collect();
    class.captured_type_params = captured_parameters
        .iter()
        .map(|parameter| parameter.semantic_name.clone())
        .collect();

    let has_parameterized_supertype = header
        .superclass
        .into_iter()
        .chain(header.interfaces.iter().copied())
        .any(|supertype| !supertype.get().type_args().is_empty());
    if own_parameters.is_empty() && !has_parameterized_supertype {
        return;
    }
    let mut supers = vec![header
        .superclass
        .map_or_else(|| Ty::obj_name(wk::any()), |superclass| superclass.get())];
    supers.extend(header.interfaces.iter().map(|interface| interface.get()));
    ir.insert_class_signature_name(
        header.classifier,
        IrGenericSig {
            type_params: own_parameters.to_vec(),
            params: Vec::new(),
            ret: None,
            supers,
        },
    );
}

pub(super) fn attach_callable_generic_facts(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    function: u32,
    ir: &mut IrFile,
) {
    let parameters = declaration_type_parameters(index, declaration);
    let callable = index.callable_for_declaration(declaration);
    let extension_receiver = callable.and_then(|callable| callable.shape.extension_receiver);
    if !parameters.is_empty()
        || extension_receiver
            .is_some_and(|receiver| crate::types::ty_mentions_any_param(receiver.get()))
    {
        let signature = index
            .signature(declaration)
            .expect("a semantic generic callable must have a pending-free signature");
        let mut signature_parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if let (Some(callable), Some(receiver)) = (callable, extension_receiver) {
            signature_parameters.insert(
                (callable.shape.context_parameter_count as usize).min(signature_parameters.len()),
                receiver.get(),
            );
        }
        ir.signatures.insert(
            function,
            IrGenericSig {
                type_params: parameters,
                params: signature_parameters,
                ret: Some(signature.result.get()),
                supers: Vec::new(),
            },
        );
    }

    let Some(classifier) = index.enclosing_classifier(declaration) else {
        return;
    };
    let class_parameter_names = index
        .classifier_type_arguments(classifier.declaration)
        .into_iter()
        .flatten()
        .filter_map(|parameter| index.type_parameter_semantic_name(*parameter))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if class_parameter_names.is_empty() {
        return;
    }
    let signature = index
        .signature(declaration)
        .expect("a member callable must have a pending-free signature");
    if signature
        .parameters
        .iter()
        .map(|parameter| parameter.get())
        .chain(std::iter::once(signature.result.get()))
        .any(|ty| crate::types::ty_mentions_param(ty, &class_parameter_names))
    {
        ir.member_semantic_sigs.insert(
            function,
            (
                signature
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                signature.result.get(),
            ),
        );
    }
}
