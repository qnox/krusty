//! Bridge-signature erasure, shared by bridge planning and the mapped builtin declarations that
//! the classpath provider realizes.

use crate::types::{stored_value_ty, Ty, TyKind};

/// Bridge-signature erasure: a type parameter becomes its bound's storage type, a nullable keeps its
/// wrapper. This is the shape a descriptor is written from, so it defines when two signatures COLLIDE.
pub fn bridge_erasure(ty: Ty) -> Ty {
    bridge_erasure_visiting(ty, &mut crate::fx_hash::HashSet::default())
}

fn bridge_erasure_visiting(ty: Ty, visiting: &mut crate::fx_hash::HashSet<&'static str>) -> Ty {
    match ty.kind() {
        // `<T : S>` where `S : Entity` erases to `Entity`, whether `S` is declared on the method
        // or on the enclosing classifier. A cycle (`T : S`, `S : T`) has no class bound.
        TyKind::TyParam(name, bound) => {
            if !visiting.insert(name.as_str()) {
                return Ty::obj("kotlin/Any");
            }
            let erased = stored_value_ty(bridge_erasure_visiting(bound, visiting));
            visiting.remove(name.as_str());
            erased
        }
        TyKind::Nullable(inner) => Ty::nullable(bridge_erasure_visiting(inner, visiting)),
        TyKind::Obj(internal, _) if internal == crate::types::TypeName::ROOT => {
            Ty::obj("kotlin/Any")
        }
        _ => ty,
    }
}

#[cfg(test)]
mod tests {
    use super::bridge_erasure;
    use crate::types::Ty;

    #[test]
    fn dependent_type_parameter_erases_to_its_class_bound_for_bridge_descriptors() {
        let owner = Ty::ty_param("S", Ty::obj("sample/Entity"));
        let method = Ty::ty_param("T", owner);

        assert_eq!(bridge_erasure(method), Ty::obj("sample/Entity"));
        assert_eq!(bridge_erasure(owner), Ty::obj("sample/Entity"));
    }

    #[test]
    fn a_repeated_type_parameter_name_stops_before_the_carried_class_bound() {
        let parameter = Ty::ty_param("T", Ty::obj("sample/Entity"));
        let repeated = Ty::ty_param("T", parameter);

        assert_eq!(bridge_erasure(repeated), Ty::obj("kotlin/Any"));
    }

    #[test]
    fn an_empty_classifier_erases_to_any() {
        assert_eq!(bridge_erasure(Ty::obj("")), Ty::obj("kotlin/Any"));
    }
}
