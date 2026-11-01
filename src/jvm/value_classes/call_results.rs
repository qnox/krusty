//! Value-class representation of checked call results.
//!
//! Providers attach the selected declaration and logical result to the call, and the JVM
//! `call_result_boundaries` pass records each relevant generic result-slot boundary and its physical
//! result before value-class lowering. This module consumes those facts in precedence order. It
//! neither looks up a callable nor infers semantics from an owner/member name.

use super::{desc, erase, repr_of_ty, Repr, Under};
use crate::fx_hash::HashMap;
use crate::ir::{Callee, ExprId, IrFile};
use crate::types::{Ty, TypeName};

/// The per-expression type facts lowering hands the representation analysis. They always travel
/// together, and none alone can classify a value-class result: `logical` says WHICH value class a
/// coerced read has after substitution, `declared` says whether the callee RETURNS one by declaration
/// (so the physical result is its erased carrier) rather than merely producing one out of a generic
/// slot (where it is a box). `List<TokenBox>.get` and `A.create(): A<String>` agree on the
/// first and differ only on the second. `property_declarations` is the same split for a property:
/// a same-file `val result: Result<T>` is declared as that value class and stores its carrier, while
/// `Box<T>.value` is declared as `T` and, once `T = Result<Boolean>`, occupies an erased slot as a box.
/// `joins` is the JVM merge type each exhaustive `when` was realized with: the representation every
/// branch value is adapted to before the join, whatever the branch produced.
#[derive(Clone, Copy)]
pub(super) struct CallTypes<'a> {
    logical: &'a crate::ir::IrNodeTypeColumn,
    declared: &'a HashMap<u32, Ty>,
    property_declarations: &'a HashMap<u32, Ty>,
    statics: &'a [crate::ir::IrStatic],
    joins: &'a HashMap<u32, Ty>,
}

impl<'a> CallTypes<'a> {
    pub(super) fn of(ir: &'a IrFile) -> Self {
        CallTypes {
            logical: ir.node_types(),
            declared: &ir.call_declared_ret,
            property_declarations: &ir.property_declaration_types,
            statics: &ir.statics,
            joins: &ir.whens.exhaustive,
        }
    }

    /// The realized JVM merge type of the exhaustive `when` at `id`, if it is one.
    pub(super) fn join_result(&self, id: u32) -> Option<Ty> {
        self.joins.get(&id).copied()
    }

    /// A same-file property declared as value class `X` whose representation is the unboxed
    /// carrier. An erased `Object` physical stamp on that read is the carrier (`kotlin.Result`),
    /// not the box a generic type-parameter slot would hold.
    pub(super) fn unboxed_declared_property(&self, id: u32, under: &Under) -> Option<Ty> {
        let declared = self.property_declarations.get(&id).copied()?;
        let classifier = declared.non_null().obj_internal()?;
        matches!(repr_of_ty(&declared, under), Repr::Unboxed(found) if found == classifier)
            .then_some(declared)
    }

    /// The value class a static's storage was realized over, so `getstatic` yields the CARRIER and
    /// not a box. `None` ⇒ this static keeps the boxed convention, or holds no value class at all.
    pub(super) fn erased_static_value_class(&self, index: u32) -> Option<TypeName> {
        self.statics
            .get(index as usize)?
            .erased_declared_ty?
            .non_null()
            .obj_internal()
    }

    pub(super) fn get(&self, id: &u32) -> Option<Ty> {
        self.logical.logical(*id)
    }

    /// The value class this call returns BY DECLARATION — so its physical result is already the erased
    /// carrier and must not be unboxed again. `None` when the callee declares no class return, or
    /// declares one that is not a value class here.
    pub(super) fn declared_value_class(&self, id: u32, under: &Under) -> Option<TypeName> {
        self.declared_result(id, under)?
            .non_null()
            .obj_internal()
            .filter(|fq| under.contains_key(fq))
    }

    /// The call's declared result as the JVM realizes it. A type parameter whose erased upper bound
    /// is a value class is declared as that value class (`T : X?` returns what `X?` returns); any
    /// other type parameter stays generic.
    pub(super) fn declared_result(&self, id: u32, under: &Under) -> Option<Ty> {
        self.declared
            .get(&id)
            .map(|declared| super::member_names::value_class_bound_occurrence(*declared, under))
    }
}

pub(super) fn representation(
    id: ExprId,
    callee: &Callee,
    under: &Under,
    types: CallTypes<'_>,
    physical: &crate::ir::IrNodeTypeColumn,
) -> Repr {
    // A callee that returns a value class BY DECLARATION hands back its erased CARRIER: that is the
    // whole classpath value-class return ABI (`fun make(): K` -> `make-<hash>(): String`), whatever
    // the underlying erases to. This checked identity distinguishes `A.create(): A<String>` from
    // `List<TokenBox>.get`: both have an `Object` descriptor, but the latter declares bare `E`.
    if let Some(declared) = types.declared_value_class(id, under) {
        return Repr::Unboxed(declared);
    }
    let Some(logical) = types.get(&id) else {
        return Repr::NotVc;
    };
    let Some(value_class) = logical
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier))
    else {
        return Repr::NotVc;
    };

    // The carrier-aware JVM boundary records the erased slot for a declaration whose result is a
    // bare type parameter (`Iterator<X>.next`, `List<X>.get`). Once a value-class declaration itself
    // has been excluded above, that generic slot holds the BOX. Its descriptor may equal the
    // carrier's descriptor, so this recorded representation fact decides before comparison.
    if physical
        .physical(id)
        .is_some_and(|physical| physical.is_erased_top())
    {
        return Repr::Boxed(value_class);
    }

    let physical_descriptor = match callee {
        Callee::Virtual {
            params: Some((_, result)),
            ..
        } => Some(desc(result)),
        Callee::Static { descriptor, .. }
        | Callee::Virtual { descriptor, .. }
        | Callee::Special { descriptor, .. } => descriptor.rsplit(')').next().map(str::to_string),
        _ => None,
    };
    // A producer with no JVM descriptor can still record that its result is this value class's
    // carrier (a primitive-array allocation is `newarray`, not a method). Absent that record, a
    // missing descriptor does not select the carrier: the result stays the box.
    if physical_descriptor.is_none()
        && physical
            .physical(id)
            .is_some_and(|recorded| recorded.non_null() == under[&value_class].non_null())
    {
        return repr_of_ty(&logical, under);
    }
    let carrier_descriptor = desc(&erase(&under[&value_class], under));
    if physical_descriptor.as_deref() == Some(carrier_descriptor.as_str()) {
        repr_of_ty(&logical, under)
    } else {
        Repr::Boxed(value_class)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Callee, IrIntrinsic};

    #[test]
    fn a_missing_descriptor_selects_the_carrier_only_when_that_result_was_recorded() {
        let value_class = crate::types::type_name("kotlin/UIntArray");
        let carrier = Ty::obj("kotlin/IntArray");
        let mut under = Under::default();
        under.insert(value_class, carrier);
        let mut logical = crate::ir::IrFile::default();
        logical.set_logical_type(0, Ty::obj_name(value_class));
        let declared = HashMap::default();
        let callee = Callee::Intrinsic {
            operation: IrIntrinsic::PrimitiveArrayNew { element: Ty::UInt },
            ret: Ty::obj_name(value_class),
        };
        let types = CallTypes {
            logical: logical.node_types(),
            declared: &declared,
            property_declarations: &declared,
            statics: &[],
            joins: &declared,
        };
        let absent = crate::ir::IrFile::default();
        assert!(matches!(
            representation(0, &callee, &under, types, absent.node_types()),
            Repr::Boxed(classifier) if classifier == value_class
        ));
        let mut recorded = crate::ir::IrFile::default();
        recorded.set_physical_type(0, carrier);
        assert!(matches!(
            representation(0, &callee, &under, types, recorded.node_types()),
            Repr::Unboxed(classifier) if classifier == value_class
        ));
    }
}
