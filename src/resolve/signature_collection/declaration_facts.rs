//! Facts a declaration's own written form decides.
//!
//! Whether a type parameter occurs projected in the return type, and which value parameter slots
//! carry a generic operand, are properties of the declaration alone. Both are computed once here
//! for the compact header and the legacy parser form so signature collection never re-derives them
//! per call site.

use super::*;

/// Normalize parser classifier flags into the provider-neutral kind a frontend plugin consumes.
pub(in crate::resolve) fn frontend_class_kind(
    flags: ClassFlags,
    is_enum: bool,
) -> crate::libraries::TypeKind {
    if flags.has(ClassFlags::ANNOTATION) {
        crate::libraries::TypeKind::Annotation
    } else if flags.has(ClassFlags::OBJECT) {
        crate::libraries::TypeKind::Object
    } else if is_enum {
        crate::libraries::TypeKind::Enum
    } else if flags.has(ClassFlags::INTERFACE) {
        crate::libraries::TypeKind::Interface
    } else {
        crate::libraries::TypeKind::Class
    }
}

fn type_ref_formal_occurrences(ty: &TypeRef, name: &str, projected: bool) -> (bool, bool) {
    let projected = projected || ty.in_projection() || ty.out_projection();
    let mut occurrences = (projected && ty.name == name, !projected && ty.name == name);
    let mut merge = |child: &TypeRef, child_projected: bool| {
        let child = type_ref_formal_occurrences(child, name, child_projected);
        occurrences.0 |= child.0;
        occurrences.1 |= child.1;
    };
    if let Some(argument) = ty.arg.as_deref() {
        merge(argument, projected);
    }
    for argument in &ty.targs {
        merge(argument, projected);
    }
    for argument in &ty.fun_params {
        merge(argument, projected);
    }
    occurrences
}

pub(in crate::resolve) fn has_projected_generic_return_hazard(
    _file: &File,
    function: &FunDecl,
) -> bool {
    has_projected_generic_return_hazard_for(
        function.ret.as_ref(),
        &function.type_params,
        function
            .receiver
            .iter()
            .chain(function.params.iter().map(|parameter| &parameter.ty)),
    )
}

pub(in crate::resolve) fn has_projected_generic_return_hazard_from_header(
    function: &StreamedCallableHeader,
) -> bool {
    has_projected_generic_return_hazard_for(
        function.explicit_result.as_ref(),
        &function.type_parameters,
        function
            .receiver
            .iter()
            .chain(function.parameters.iter().map(|parameter| &parameter.ty)),
    )
}

fn has_projected_generic_return_hazard_for<'a>(
    result: Option<&TypeRef>,
    type_parameters: &[String],
    inputs: impl Iterator<Item = &'a TypeRef>,
) -> bool {
    let Some(ret) = result else {
        return false;
    };
    if !type_parameters
        .iter()
        .any(|parameter| parameter == &ret.name)
    {
        return false;
    }
    let mut occurrences = (false, false);
    for ty in inputs {
        let here = type_ref_formal_occurrences(ty, &ret.name, false);
        occurrences.0 |= here.0;
        occurrences.1 |= here.1;
    }
    occurrences.0 && !occurrences.1
}

pub(in crate::resolve) fn generic_value_operand_slots(
    function: &FunDecl,
    owner_type_params: &[String],
) -> Vec<u32> {
    generic_value_operand_slots_for(
        function.receiver.as_ref(),
        function
            .params
            .iter()
            .map(|parameter| (&parameter.ty, parameter.is_vararg)),
        &function.type_params,
        owner_type_params,
    )
}

pub(in crate::resolve) fn generic_value_operand_slots_from_header(
    function: &StreamedCallableHeader,
    owner_type_params: &[String],
) -> Vec<u32> {
    generic_value_operand_slots_for(
        function.receiver.as_ref(),
        function
            .parameters
            .iter()
            .map(|parameter| (&parameter.ty, parameter.is_vararg)),
        &function.type_parameters,
        owner_type_params,
    )
}

fn generic_value_operand_slots_for<'a>(
    receiver: Option<&TypeRef>,
    parameters: impl Iterator<Item = (&'a TypeRef, bool)>,
    type_parameters: &[String],
    owner_type_params: &[String],
) -> Vec<u32> {
    let is_bare_formal = |ty: &TypeRef| {
        !ty.nullable()
            && ty.arg.is_none()
            && ty.targs.is_empty()
            && ty.fun_params.is_empty()
            && type_parameters
                .iter()
                .chain(owner_type_params)
                .any(|parameter| parameter == &ty.name)
    };
    let mut slots = Vec::new();
    if receiver.is_some_and(is_bare_formal) {
        slots.push(0);
    }
    slots.extend(
        parameters
            .enumerate()
            .filter(|(_, (ty, is_vararg))| !is_vararg && is_bare_formal(ty))
            .map(|(index, _)| index as u32 + 1),
    );
    slots
}
