//! Array allocation declarations authenticated before publishing their provider identities.

use crate::libraries::{GenericReturnPolicy, LibraryMember, LibraryType, TypeKind};
use crate::types::{SemanticCallRole, Ty, TypeName};

pub(in super::super) fn realize_array_constructor(
    owner: TypeName,
    compiler_builtin_owner: bool,
    shape: &LibraryType,
    member: &mut LibraryMember,
) {
    if !compiler_builtin_owner || !is_size_constructor(member, owner) {
        return;
    }
    if owner == crate::types::wk::array() {
        realize_reference_array_constructor(shape, member);
    } else if is_primitive_array_class(owner, shape) && member.generic_sig.is_none() {
        member.semantic_role = Some(SemanticCallRole::KotlinArrayAllocation);
    }
}

/// `<init>(size: Int)` of `owner`, with nothing else a plain allocation could not honour.
fn is_size_constructor(member: &LibraryMember, owner: TypeName) -> bool {
    member.owner == Some(owner)
        && member.name == "<init>"
        && member.params == [Ty::Int]
        && member.ret == Ty::Unit
        && member.context_count == 0
        && member.call_sig.vararg_index.is_none()
        && !member.call_sig.param_defaults.contains(&true)
        && !member.suspend()
        && !member.is_operator()
        && !member.is_infix()
}

/// A compiler-provided specialized array of a signed primitive (`IntArray`, ...): a final,
/// non-generic, top-level class. An unsigned array is a value class over one of these, and its
/// constructor is that class's ordinary constructor.
fn is_primitive_array_class(owner: TypeName, shape: &LibraryType) -> bool {
    crate::types::prim_array_element(owner).is_some_and(|element| !element.is_unsigned())
        && shape.kind == TypeKind::Class
        && shape.value_declaration.is_none()
        && shape.own_type_parameter_count == 0
        && shape.type_params().is_empty()
        && shape.outer_instance.is_none()
}

fn realize_reference_array_constructor(shape: &LibraryType, member: &mut LibraryMember) {
    if shape.kind != TypeKind::Class
        || shape.own_type_parameter_count != 1
        || shape.type_params().len() != 1
        || shape.outer_instance.is_some()
    {
        return;
    }
    let Some(signature) = &member.generic_sig else {
        return;
    };
    let [bounds] = shape.type_param_bounds().as_slice() else {
        return;
    };
    // An omitted class upper bound means Any?, exactly as constructor normalization records it.
    let bound = match bounds.as_slice() {
        [] => Ty::nullable(Ty::obj_name(crate::types::wk::any())),
        [bound] => *bound,
        _ => return,
    };
    if signature.formals == *shape.type_params()
        && signature.formal_bounds == *shape.type_param_bounds()
        && signature.receiver.is_none()
        && signature.params == [Ty::Int]
        && signature.ret == Ty::array(Ty::ty_param(&shape.type_params()[0], bound))
        && signature.return_policy == GenericReturnPolicy::Exact
    {
        member.semantic_role = Some(SemanticCallRole::KotlinArrayAllocation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration() -> (LibraryType, LibraryMember) {
        let mut shape = LibraryType::declaration_header();
        shape.kind = TypeKind::Class;
        shape.own_type_parameter_count = 1;
        shape.type_parameters = crate::types::TypeParameters::new(
            vec!["Element".into()],
            vec![vec![]],
            vec![crate::types::TypeVariance::Invariant],
        );
        let mut constructor =
            LibraryMember::new("<init>".into(), vec![Ty::Int], Ty::Unit, String::new());
        constructor.owner = Some(crate::types::wk::array());
        constructor.generic_sig = crate::libraries::constructor_generic_signature(
            crate::types::wk::array(),
            &shape,
            &constructor,
        );
        (shape, constructor)
    }

    #[test]
    fn array_allocation_requires_authority_and_survives_constructor_normalization() {
        let owner = crate::types::wk::array();
        for bound in [
            Vec::new(),
            vec![Ty::nullable(Ty::obj_name(crate::types::wk::any()))],
        ] {
            let (mut shape, mut constructor) = declaration();
            shape.type_parameters.map_bounds(|bounds| bounds[0] = bound);
            constructor.generic_sig = None;
            constructor.generic_sig =
                crate::libraries::constructor_generic_signature(owner, &shape, &constructor);
            let mut untrusted = constructor.clone();
            realize_array_constructor(owner, false, &shape, &mut untrusted);
            assert_eq!(untrusted.semantic_role, None);
            realize_array_constructor(owner, true, &shape, &mut constructor);
            assert_eq!(
                constructor.semantic_role,
                Some(SemanticCallRole::KotlinArrayAllocation)
            );
            assert_eq!(
                crate::libraries::LibraryCallable::constructor(owner, &constructor).semantic_role,
                constructor.semantic_role
            );
        }
    }

    #[test]
    fn a_different_constructor_or_class_application_is_not_array_allocation() {
        let (shape, original) = declaration();
        let mut wrong_parameter = original.clone();
        wrong_parameter.params = vec![Ty::Long];
        let mut contextual = original.clone();
        contextual.context_count = 1;
        let mut wrong_application = original.clone();
        wrong_application.generic_sig.as_mut().unwrap().ret = Ty::array(Ty::String);
        let mut extension = original.clone();
        extension.generic_sig.as_mut().unwrap().receiver = Some(Ty::String);
        let mut wrong_owner = original.clone();
        wrong_owner.owner = Some(crate::types::type_name("user/Array"));
        for mut constructor in [
            wrong_parameter,
            contextual,
            wrong_application,
            extension,
            wrong_owner,
        ] {
            realize_array_constructor(crate::types::wk::array(), true, &shape, &mut constructor);
            assert_eq!(constructor.semantic_role, None);
        }
        let mut interface = shape.clone();
        interface.kind = TypeKind::Interface;
        let mut nongeneric = shape.clone();
        nongeneric.own_type_parameter_count = 0;
        for malformed in [interface, nongeneric] {
            let mut constructor = original.clone();
            realize_array_constructor(
                crate::types::wk::array(),
                true,
                &malformed,
                &mut constructor,
            );
            assert_eq!(constructor.semantic_role, None);
        }
    }

    #[test]
    fn a_signed_primitive_array_size_constructor_is_an_allocation() {
        let primitive = |owner: TypeName| {
            let mut shape = LibraryType::declaration_header();
            shape.kind = TypeKind::Class;
            let mut constructor =
                LibraryMember::new("<init>".into(), vec![Ty::Int], Ty::Unit, String::new());
            constructor.owner = Some(owner);
            (shape, constructor)
        };
        for element in [Ty::Int, Ty::Long, Ty::Boolean, Ty::Char, Ty::Double] {
            let owner = Ty::array(element).obj_internal().unwrap();
            let (shape, mut constructor) = primitive(owner);
            let mut untrusted = constructor.clone();
            realize_array_constructor(owner, false, &shape, &mut untrusted);
            assert_eq!(untrusted.semantic_role, None);
            realize_array_constructor(owner, true, &shape, &mut constructor);
            assert_eq!(
                constructor.semantic_role,
                Some(SemanticCallRole::KotlinArrayAllocation),
                "{element:?}"
            );
        }
        let int_array = Ty::array(Ty::Int).obj_internal().unwrap();
        let (shape, original) = primitive(int_array);
        let mut initializing = original.clone();
        initializing.params.push(Ty::obj("kotlin/Function1"));
        let mut long_size = original.clone();
        long_size.params = vec![Ty::Long];
        for mut constructor in [initializing, long_size] {
            realize_array_constructor(int_array, true, &shape, &mut constructor);
            assert_eq!(constructor.semantic_role, None);
        }
        let mut nested = shape.clone();
        nested.outer_instance = Some(crate::types::type_name("kotlin/Outer"));
        let mut constructor = original.clone();
        realize_array_constructor(int_array, true, &nested, &mut constructor);
        assert_eq!(constructor.semantic_role, None);
        // An unsigned array is a value class over a signed one; its constructor is its own.
        let unsigned = Ty::array(Ty::UInt).obj_internal().unwrap();
        let (shape, mut constructor) = primitive(unsigned);
        realize_array_constructor(unsigned, true, &shape, &mut constructor);
        assert_eq!(constructor.semantic_role, None);
    }
}
