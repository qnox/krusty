//! Which value classes the JVM carries through its value-class box and carrier machinery.
//!
//! The IR's value-class table is semantic: it holds every checked value-class declaration,
//! including the ones the type model carries as a native scalar (the unsigned integers). The JVM
//! gives a native scalar a primitive slot and boxes it through its wrapper like any other scalar,
//! so the questions "box this through `box-impl`" and "unbox to the declared carrier" exclude it.
//! Callable naming does not ask these questions: kotlinc mangles by the semantic identity.

use crate::ir::IrFile;
use crate::types::{Ty, TypeName};

/// Whether the type model carries `classifier` as a native scalar rather than a class reference.
pub(super) fn has_native_carrier(classifier: TypeName) -> bool {
    Ty::obj_name(classifier).is_jvm_scalar()
}

/// Whether the JVM represents `classifier` as a value class with its own box and carrier.
pub(crate) fn is_boxed_value_class(ir: &IrFile, classifier: TypeName) -> bool {
    ir.is_value_class_name(classifier) && !has_native_carrier(classifier)
}

/// The declared underlying type of a value class the JVM boxes through its own `box-impl`.
pub(crate) fn boxed_value_class_underlying(ir: &IrFile, classifier: TypeName) -> Option<Ty> {
    if has_native_carrier(classifier) {
        return None;
    }
    ir.value_class_underlying_name(classifier)
}

/// Internal name → declared underlying of each value class this file declares, before recursive
/// value-class erasure. A generic underlying property uses its declared upper bound: `S<T : String>`
/// carries `String`, while `V<T : Int>` carries `int`. Keeping the unbound `TyParam` here would
/// incorrectly force an Object slot and later descriptor code would independently specialize the
/// same bound, leaving boxing and null guards inconsistent with the emitted method descriptor. A
/// nullable occurrence (`val x: T?`) keeps its `?` on that bound, as kotlinc's `getUnderlyingType`
/// keeps a nullable type parameter: `X<T : Any>(val x: T?)` carries `Any?`, so `X?` is boxed.
pub(super) fn declared_underlyings(ir: &IrFile) -> crate::value_classes::UnderlyingTypes {
    ir.classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields.first().map(|f| {
                let u = f
                    .type_param
                    .as_ref()
                    .map(|name| {
                        let bound = c
                            .type_param_bounds
                            .iter()
                            .find(|(candidate, _)| candidate == name)
                            .map(|(_, bound)| *bound)
                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                        if f.ty.is_nullable() {
                            Ty::nullable(bound)
                        } else {
                            bound
                        }
                    })
                    .unwrap_or(f.ty)
                    .canonical_semantic();
                (c.fq_name, u)
            })
        })
        .collect()
}

/// The carrier of a value class the JVM boxes through its own `box-impl`: its underlying erased as
/// the JVM erases it, through nested value classes but stopping at a nullable one the JVM keeps
/// boxed (`NZ1(val nz: Z?)` over `Z(val x: Int)` carries `Z`, not `int`).
pub(crate) fn boxed_value_class_carrier(ir: &IrFile, classifier: TypeName) -> Option<Ty> {
    if has_native_carrier(classifier) {
        return None;
    }
    let mut declarations = declared_underlyings(ir);
    for name in ir.value_class_names() {
        if let (std::collections::hash_map::Entry::Vacant(entry), Some(declared)) = (
            declarations.entry(name),
            ir.value_class_underlying_name(name),
        ) {
            entry.insert(declared);
        }
    }
    let underlying = *declarations.get(&classifier)?;
    Some(super::erase(&underlying, &declarations))
}

/// Every value class this IR knows that the JVM boxes through its own `box-impl`.
pub(crate) fn boxed_value_class_names(ir: &IrFile) -> impl Iterator<Item = TypeName> + '_ {
    ir.value_class_names()
        .filter(|&classifier| !has_native_carrier(classifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_scalar_value_class_is_semantic_but_not_boxed() {
        let uint = crate::types::type_name("kotlin/UInt");
        let count = crate::types::type_name("fixture/Count");
        let mut ir = IrFile::default();
        ir.insert_external_value_class_name(uint, Ty::Int);
        ir.insert_external_value_class_name(count, Ty::Int);

        assert_eq!(ir.value_class_underlying_name(uint), Some(Ty::Int));
        assert!(!is_boxed_value_class(&ir, uint));
        assert_eq!(boxed_value_class_underlying(&ir, uint), None);
        assert!(is_boxed_value_class(&ir, count));
        assert_eq!(boxed_value_class_underlying(&ir, count), Some(Ty::Int));
        assert_eq!(
            boxed_value_class_names(&ir).collect::<Vec<_>>(),
            vec![count]
        );
    }
}
