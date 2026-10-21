//! Exact declarations whose platform-owned Throwable storage the target realizes.
//!
//! Archive authority is an input fact, never inferred from a package or manifest spelling. Attach
//! the operation before publishing the selected constructor/accessor's external identity.

use crate::libraries::{
    CompilerIntrinsic, LibraryMember, MemberRealization, PropKind, PropertyInfo,
};
use crate::types::{type_name, Ty, TypeName};

pub(in crate::klib_libraries) fn realize_constructor(
    owner: TypeName,
    compiler_builtin_owner: bool,
    member: &mut LibraryMember,
) {
    if !compiler_builtin_owner
        || owner != type_name("kotlin/Throwable")
        || member.owner != Some(owner)
        || member.name != "<init>"
        || member.ret != Ty::Unit
        || member.generic_sig.is_some()
        || member.context_count != 0
        || member.call_sig.vararg_index.is_some()
        || member.call_sig.param_defaults.contains(&true)
        || member.suspend()
        || member.is_operator()
        || member.is_infix()
    {
        return;
    }
    let message = match member.params.as_slice() {
        [] => false,
        [parameter] if *parameter == Ty::nullable(Ty::String) => true,
        _ => return,
    };
    member.realization =
        MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor { message });
}

pub(in crate::klib_libraries) fn realize_throwable_property(
    owner: TypeName,
    compiler_builtin_owner: bool,
    property: &mut PropertyInfo,
) {
    if compiler_builtin_owner
        && owner == type_name("kotlin/Throwable")
        && property.owner == owner
        && property.name == "message"
        && property.kind == PropKind::Member
        && property.receiver.is_none()
        && property.formals.is_empty()
        && property.associated_classifier.is_none()
        && property.context_count == 0
        && property.setter.is_none()
        && !property.is_const
        && property.ty == Ty::nullable(Ty::String)
        && property.getter.params.is_empty()
        && property.getter.context_count == 0
        && property.getter.generic_sig.is_none()
        && property.getter.source_receiver.is_none()
        && !property.getter.owner_is_interface
        && !property.getter.is_abstract
        && property.getter.ret == property.ty
        && !property.getter.suspend
    {
        property.getter.compiler_intrinsic = Some(CompilerIntrinsic::ThrowableMessage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constructor(params: Vec<Ty>) -> LibraryMember {
        let mut member = LibraryMember::new("<init>".into(), params, Ty::Unit, String::new());
        member.owner = Some(type_name("kotlin/Throwable"));
        member
    }

    fn message_property() -> PropertyInfo {
        use crate::libraries::{
            declared_property, property_parameter_identities, CallablePlacement,
            PropertyAccessorNames, TypeParameterIdentities,
        };
        use crate::metadata::semantic::{
            KotlinFunctionTypeShape, KotlinModality, KotlinProperty, KotlinType,
        };
        use crate::types::Visibility;

        let declaration = KotlinProperty {
            name: "message".into(),
            receiver: None,
            context_params: Vec::new(),
            ty: KotlinType::Class {
                internal: "kotlin/String".into(),
                args: Vec::new(),
                nullable: true,
                shape: KotlinFunctionTypeShape::default(),
            },
            formals: Vec::new(),
            visibility: Visibility::Public,
            modality: KotlinModality::Open,
            setter_visibility: Visibility::Public,
            setter_parameter_name: None,
            is_var: false,
            is_const: false,
            is_expect: false,
            is_static: false,
            context_count: 0,
            context_param_names: Vec::new(),
            context_kinds: Vec::new(),
            constant: None,
            annotations: Vec::new(),
            return_value_status: Default::default(),
        };
        declared_property(
            CallablePlacement::Member {
                owner: type_name("kotlin/Throwable"),
                owner_is_interface: false,
            },
            &declaration,
            &property_parameter_identities(&declaration).expect("the getter is signable"),
            &TypeParameterIdentities::default(),
            PropertyAccessorNames {
                getter: "<get-message>",
                setter: None,
            },
            &Default::default(),
        )
    }

    #[test]
    fn throwable_message_requires_an_authenticated_exact_immutable_member_property() {
        let owner = type_name("kotlin/Throwable");
        let original = message_property();
        let mut trusted = original.clone();
        realize_throwable_property(owner, true, &mut trusted);
        assert_eq!(
            trusted.getter.compiler_intrinsic,
            Some(CompilerIntrinsic::ThrowableMessage)
        );
        let mut identical_untrusted = original.clone();
        realize_throwable_property(owner, false, &mut identical_untrusted);
        assert_eq!(identical_untrusted.getter.compiler_intrinsic, None);

        let mut wrong_type = original.clone();
        wrong_type.ty = Ty::String;
        let mut mutable = original.clone();
        mutable.setter = Some(Box::new(original.getter.clone()));
        let mut extension = original.clone();
        extension.kind = PropKind::Extension;
        extension.receiver = Some(Ty::obj_name(owner));
        let mut context = original.clone();
        context.context_count = 1;
        let mut generic = original.clone();
        generic.formals.push("T".into());
        let mut wrong_getter = original.clone();
        wrong_getter.getter.ret = Ty::Int;
        for mut property in [
            wrong_type,
            mutable,
            extension,
            context,
            generic,
            wrong_getter,
        ] {
            realize_throwable_property(owner, true, &mut property);
            assert_eq!(property.getter.compiler_intrinsic, None);
        }
        let mut other_owner = original;
        realize_throwable_property(type_name("user/Throwable"), true, &mut other_owner);
        assert_eq!(other_owner.getter.compiler_intrinsic, None);
    }

    #[test]
    fn throwable_constructors_require_archive_authority_and_the_complete_signature() {
        let owner = type_name("kotlin/Throwable");
        for (params, message) in [(Vec::new(), false), (vec![Ty::nullable(Ty::String)], true)] {
            let mut trusted = constructor(params.clone());
            realize_constructor(owner, true, &mut trusted);
            assert_eq!(
                trusted.realization,
                MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor { message })
            );
            let mut identical_untrusted = constructor(params);
            realize_constructor(owner, false, &mut identical_untrusted);
            assert_eq!(identical_untrusted.realization, MemberRealization::Dispatch);
        }
        for params in [
            vec![Ty::String],
            vec![Ty::nullable(Ty::obj_name(owner))],
            vec![Ty::nullable(Ty::String), Ty::nullable(Ty::obj_name(owner))],
        ] {
            let mut member = constructor(params);
            realize_constructor(owner, true, &mut member);
            assert_eq!(member.realization, MemberRealization::Dispatch);
        }
        let mut wrong_owner = constructor(Vec::new());
        realize_constructor(type_name("user/Throwable"), true, &mut wrong_owner);
        assert_eq!(wrong_owner.realization, MemberRealization::Dispatch);
        let mut wrong_result = constructor(Vec::new());
        wrong_result.ret = Ty::Int;
        realize_constructor(owner, true, &mut wrong_result);
        assert_eq!(wrong_result.realization, MemberRealization::Dispatch);
        let mut context = constructor(vec![Ty::nullable(Ty::String)]);
        context.context_count = 1;
        realize_constructor(owner, true, &mut context);
        assert_eq!(context.realization, MemberRealization::Dispatch);
        let mut vararg = constructor(vec![Ty::nullable(Ty::String)]);
        vararg.call_sig.vararg_index = Some(0);
        realize_constructor(owner, true, &mut vararg);
        assert_eq!(vararg.realization, MemberRealization::Dispatch);
    }
}
