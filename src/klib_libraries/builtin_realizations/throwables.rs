//! Exact declarations whose platform-owned Throwable storage the target realizes.
//!
//! Archive authority is an input fact, never inferred from a package or manifest spelling. Attach
//! the operation before publishing the selected constructor/accessor's external identity.

use crate::libraries::{
    CompilerIntrinsic, LibraryMember, MemberRealization, PropKind, PropertyInfo,
    RuntimeExceptionLayout,
};
use crate::types::{type_name, Ty, TypeName};

pub(in super::super) fn realize_constructor(
    owner: TypeName,
    compiler_builtin_owner: bool,
    layout: Option<RuntimeExceptionLayout>,
    member: &mut LibraryMember,
) {
    let Some(layout) = layout else { return };
    if !compiler_builtin_owner
        || layout.kind.classifier() != owner
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
    member.realization = MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor {
        kind: layout.kind,
        message,
    });
}

pub(in super::super) fn realize_throwable_property(
    owner: TypeName,
    compiler_builtin_owner: bool,
    property: &mut PropertyInfo,
) {
    // `message: String?` and `cause: Throwable?`, the two fields the platform's storage holds.
    let (intrinsic, ty) = match property.name.as_str() {
        "message" => (
            CompilerIntrinsic::ThrowableMessage,
            Ty::nullable(Ty::String),
        ),
        "cause" => (
            CompilerIntrinsic::ThrowableCause,
            Ty::nullable(Ty::obj_name(owner)),
        ),
        _ => return,
    };
    if compiler_builtin_owner
        && owner == type_name("kotlin/Throwable")
        && property.owner == owner
        && property.kind == PropKind::Member
        && property.receiver.is_none()
        && property.formals.is_empty()
        && property.associated_classifier.is_none()
        && property.context_count == 0
        && property.setter.is_none()
        && !property.is_const
        && property.ty == ty
        && property.getter.params.is_empty()
        && property.getter.context_count == 0
        && property.getter.generic_sig.is_none()
        && property.getter.source_receiver.is_none()
        && !property.getter.owner_is_interface
        && !property.getter.is_abstract
        && property.getter.ret == property.ty
        && !property.getter.suspend
    {
        property.getter.compiler_intrinsic = Some(intrinsic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::RuntimeExceptionKind;

    fn throwable_layout() -> Option<RuntimeExceptionLayout> {
        Some(RuntimeExceptionLayout {
            kind: RuntimeExceptionKind::Throwable,
            parent: None,
        })
    }

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
            getter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
                Visibility::Public,
                KotlinModality::Open,
            ),
            setter_modifiers: crate::metadata::semantic::KotlinAccessorModifiers::default_for(
                Visibility::Public,
                KotlinModality::Open,
            ),
            is_external: false,
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
    fn throwable_cause_requires_an_authenticated_member_of_its_exact_type() {
        let owner = type_name("kotlin/Throwable");
        let mut cause = message_property();
        cause.name = "cause".into();
        cause.ty = Ty::nullable(Ty::obj_name(owner));
        cause.getter.ret = cause.ty;

        let mut trusted = cause.clone();
        realize_throwable_property(owner, true, &mut trusted);
        assert_eq!(
            trusted.getter.compiler_intrinsic,
            Some(CompilerIntrinsic::ThrowableCause)
        );
        let mut untrusted = cause.clone();
        realize_throwable_property(owner, false, &mut untrusted);
        assert_eq!(untrusted.getter.compiler_intrinsic, None);

        let mut text = cause.clone();
        text.ty = Ty::nullable(Ty::String);
        text.getter.ret = text.ty;
        let mut other_name = cause;
        other_name.name = "suppressed".into();
        for mut property in [text, other_name] {
            realize_throwable_property(owner, true, &mut property);
            assert_eq!(property.getter.compiler_intrinsic, None);
        }
    }

    #[test]
    fn throwable_constructors_require_archive_authority_and_the_complete_signature() {
        let owner = type_name("kotlin/Throwable");
        for (params, message) in [(Vec::new(), false), (vec![Ty::nullable(Ty::String)], true)] {
            let mut trusted = constructor(params.clone());
            realize_constructor(owner, true, throwable_layout(), &mut trusted);
            assert_eq!(
                trusted.realization,
                MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor {
                    kind: RuntimeExceptionKind::Throwable,
                    message
                })
            );
            let mut identical_untrusted = constructor(params);
            realize_constructor(owner, false, throwable_layout(), &mut identical_untrusted);
            assert_eq!(identical_untrusted.realization, MemberRealization::Dispatch);
        }
        for params in [
            vec![Ty::String],
            vec![Ty::nullable(Ty::obj_name(owner))],
            vec![Ty::nullable(Ty::String), Ty::nullable(Ty::obj_name(owner))],
        ] {
            let mut member = constructor(params);
            realize_constructor(owner, true, throwable_layout(), &mut member);
            assert_eq!(member.realization, MemberRealization::Dispatch);
        }
        let mut wrong_owner = constructor(Vec::new());
        realize_constructor(
            type_name("user/Throwable"),
            true,
            throwable_layout(),
            &mut wrong_owner,
        );
        assert_eq!(wrong_owner.realization, MemberRealization::Dispatch);
        let mut wrong_result = constructor(Vec::new());
        wrong_result.ret = Ty::Int;
        realize_constructor(owner, true, throwable_layout(), &mut wrong_result);
        assert_eq!(wrong_result.realization, MemberRealization::Dispatch);
        let mut context = constructor(vec![Ty::nullable(Ty::String)]);
        context.context_count = 1;
        realize_constructor(owner, true, throwable_layout(), &mut context);
        assert_eq!(context.realization, MemberRealization::Dispatch);
        let mut vararg = constructor(vec![Ty::nullable(Ty::String)]);
        vararg.call_sig.vararg_index = Some(0);
        realize_constructor(owner, true, throwable_layout(), &mut vararg);
        assert_eq!(vararg.realization, MemberRealization::Dispatch);
    }

    #[test]
    fn selected_exception_constructors_keep_their_declaring_class_authority() {
        let owner = type_name("kotlin/IllegalStateException");
        let layout = Some(RuntimeExceptionLayout {
            kind: RuntimeExceptionKind::IllegalState,
            parent: Some(RuntimeExceptionKind::RuntimeException.classifier()),
        });
        for (params, message) in [(Vec::new(), false), (vec![Ty::nullable(Ty::String)], true)] {
            let mut trusted = constructor(params.clone());
            trusted.owner = Some(owner);
            realize_constructor(owner, true, layout, &mut trusted);
            assert!(matches!(
                trusted.realization,
                MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor {
                    kind: RuntimeExceptionKind::IllegalState,
                    message: actual,
                }) if actual == message
            ));
            let mut untrusted = constructor(params);
            untrusted.owner = Some(owner);
            realize_constructor(owner, false, layout, &mut untrusted);
            assert_eq!(untrusted.realization, MemberRealization::Dispatch);
        }
    }

    #[test]
    fn illegal_argument_constructors_keep_their_declaring_class_authority() {
        let owner = type_name("kotlin/IllegalArgumentException");
        let layout = Some(RuntimeExceptionLayout {
            kind: RuntimeExceptionKind::IllegalArgument,
            parent: Some(RuntimeExceptionKind::RuntimeException.classifier()),
        });
        for (params, message) in [(Vec::new(), false), (vec![Ty::nullable(Ty::String)], true)] {
            let mut trusted = constructor(params.clone());
            trusted.owner = Some(owner);
            realize_constructor(owner, true, layout, &mut trusted);
            assert!(matches!(
                trusted.realization,
                MemberRealization::Intrinsic(CompilerIntrinsic::ThrowableConstructor {
                    kind: RuntimeExceptionKind::IllegalArgument,
                    message: actual,
                }) if actual == message
            ));
            let mut untrusted = constructor(params);
            untrusted.owner = Some(owner);
            realize_constructor(owner, false, layout, &mut untrusted);
            assert_eq!(untrusted.realization, MemberRealization::Dispatch);
        }
    }

    #[test]
    fn constructor_authority_requires_the_selected_storage_role_and_overload() {
        let owner = RuntimeExceptionKind::IllegalState.classifier();
        let valid = RuntimeExceptionLayout {
            kind: RuntimeExceptionKind::IllegalState,
            parent: Some(RuntimeExceptionKind::RuntimeException.classifier()),
        };
        for layout in [None, throwable_layout()] {
            let mut member = constructor(vec![Ty::nullable(Ty::String)]);
            member.owner = Some(owner);
            realize_constructor(owner, true, layout, &mut member);
            assert_eq!(member.realization, MemberRealization::Dispatch);
        }
        for params in [
            vec![Ty::String],
            vec![Ty::nullable(Ty::obj("kotlin/Throwable"))],
            vec![
                Ty::nullable(Ty::String),
                Ty::nullable(Ty::obj("kotlin/Throwable")),
            ],
        ] {
            let mut member = constructor(params);
            member.owner = Some(owner);
            realize_constructor(owner, true, Some(valid), &mut member);
            assert_eq!(member.realization, MemberRealization::Dispatch);
        }
        let mut member = constructor(vec![Ty::nullable(Ty::String)]);
        member.owner = Some(owner);
        member.call_sig.param_defaults = crate::libraries::ParamFlags::repeat(true, 1);
        realize_constructor(owner, true, Some(valid), &mut member);
        assert_eq!(member.realization, MemberRealization::Dispatch);
        let mut wrong_owner = constructor(vec![Ty::nullable(Ty::String)]);
        realize_constructor(owner, true, Some(valid), &mut wrong_owner);
        assert_eq!(wrong_owner.realization, MemberRealization::Dispatch);
    }
}
