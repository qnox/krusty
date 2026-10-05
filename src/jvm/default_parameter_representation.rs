//! JVM representation facts shared by default-stub realization and emission.
//!
//! A primitive-bounded Kotlin type parameter has two physical forms at this boundary: its real
//! method uses the primitive bound, and its `$default` stub uses the corresponding JDK wrapper when
//! the parameter has a default. kotlinc's stub takes a parameter without one as the primitive:
//! `fun <C : Char> f(a: C, b: C = a)` gives `f$default(char, Character, int, Object)`.

use crate::ir::IrFile;
use crate::jvm::physical_type::ir_ty_to_jvm;
use crate::types::Ty;

/// Record, with the parameters each `$default` stub takes boxed, every defaulted one that is a
/// primitive-bounded type parameter, as its JDK wrapper.
pub(super) fn record_defaulted_primitive_bounds(ir: &mut IrFile) {
    let mut boxed = Vec::new();
    for (&function, parameters) in &ir.fn_params {
        let Some(defaults) = &parameters.defaults else {
            continue;
        };
        let declaration = &ir.functions[function as usize];
        // The defaults are recorded before a value class's member becomes static over its
        // receiver.
        let receiver =
            usize::from(declaration.is_static && declaration.dispatch_receiver.is_some());
        let defaulted = (0..declaration.params.len())
            .filter(|&index| {
                index
                    .checked_sub(receiver)
                    .and_then(|index| defaults.get(index))
                    .is_some_and(Option::is_some)
            })
            .collect::<Vec<_>>();
        for (index, wrapper) in defaulted_primitive_bounds(&declaration.params, &defaulted) {
            boxed.push((function, index, wrapper));
        }
    }
    for (function, index, wrapper) in boxed {
        ir.default_stub_boxed_params
            .entry(function)
            .or_default()
            .push((index, wrapper));
    }
}

/// Each of the `defaulted` positions of `parameters` that is a primitive-bounded type parameter,
/// with the JDK wrapper the `$default` stub takes it as.
pub(super) fn defaulted_primitive_bounds(
    parameters: &[Ty],
    defaulted: &[usize],
) -> Vec<(usize, Ty)> {
    defaulted
        .iter()
        .filter_map(|&index| {
            let (_, wrapper) = primitive_bounded_type_parameter(*parameters.get(index)?)?;
            Some((index, wrapper))
        })
        .collect()
}

/// `(primitive, JDK wrapper)` when `declared` is a non-null type parameter bounded by a JVM
/// primitive.
pub(super) fn primitive_bounded_type_parameter(declared: Ty) -> Option<(Ty, Ty)> {
    let Ty::TyParam(_, bound) = declared.non_null() else {
        return None;
    };
    if declared.is_nullable() {
        return None;
    }
    let primitive = ir_ty_to_jvm(bound);
    primitive_wrapper(primitive).map(|wrapper| (primitive, wrapper))
}

pub(super) fn primitive_wrapper(primitive: Ty) -> Option<Ty> {
    let internal = match primitive {
        Ty::Boolean => "java/lang/Boolean",
        Ty::Byte => "java/lang/Byte",
        Ty::Short => "java/lang/Short",
        Ty::Char => "java/lang/Character",
        Ty::Int => "java/lang/Integer",
        Ty::Long => "java/lang/Long",
        Ty::Float => "java/lang/Float",
        Ty::Double => "java/lang/Double",
        _ => return None,
    };
    Some(Ty::obj(internal))
}

pub(super) fn primitive_unbox_method(primitive: Ty) -> Option<&'static str> {
    match primitive {
        Ty::Boolean => Some("booleanValue"),
        Ty::Byte => Some("byteValue"),
        Ty::Short => Some("shortValue"),
        Ty::Char => Some("charValue"),
        Ty::Int => Some("intValue"),
        Ty::Long => Some("longValue"),
        Ty::Float => Some("floatValue"),
        Ty::Double => Some("doubleValue"),
        _ => None,
    }
}
