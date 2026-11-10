use super::*;
use crate::types::type_name;

fn name_property(name: &str) -> PropertyInfo {
    use crate::libraries::{
        declared_property, property_parameter_identities, CallablePlacement, PropertyAccessorNames,
        TypeParameterIdentities,
    };
    use crate::metadata::semantic::{
        KotlinAccessorModifiers, KotlinFunctionTypeShape, KotlinModality, KotlinProperty,
        KotlinType,
    };
    use crate::types::Visibility;

    let declaration = KotlinProperty {
        name: name.into(),
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
        modality: KotlinModality::Abstract,
        getter_modifiers: KotlinAccessorModifiers::default_for(
            Visibility::Public,
            KotlinModality::Abstract,
        ),
        setter_modifiers: KotlinAccessorModifiers::default_for(
            Visibility::Public,
            KotlinModality::Abstract,
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
    let getter = format!("<get-{name}>");
    declared_property(
        CallablePlacement::Member {
            owner: wk::kclass(),
            owner_is_interface: true,
        },
        &declaration,
        &property_parameter_identities(&declaration).expect("the getter is signable"),
        &TypeParameterIdentities::default(),
        PropertyAccessorNames {
            getter: &getter,
            setter: None,
        },
        &Default::default(),
    )
}

#[test]
fn class_name_getters_require_archive_authority_and_the_exact_declaration() {
    for (name, kind) in [
        ("simpleName", ClassNameKind::Simple),
        ("qualifiedName", ClassNameKind::Qualified),
    ] {
        let original = name_property(name);
        let mut trusted = original.clone();
        realize_class_name_property(wk::kclass(), true, &mut trusted);
        assert_eq!(
            trusted.getter.compiler_intrinsic,
            Some(CompilerIntrinsic::ClassName(kind))
        );
        let mut untrusted = original.clone();
        realize_class_name_property(wk::kclass(), false, &mut untrusted);
        assert_eq!(untrusted.getter.compiler_intrinsic, None);
        let mut other_owner = original.clone();
        realize_class_name_property(type_name("user/KClass"), true, &mut other_owner);
        assert_eq!(other_owner.getter.compiler_intrinsic, None);
        let mut wrong_type = original.clone();
        wrong_type.ty = Ty::String;
        let mut extension = original.clone();
        extension.kind = PropKind::Extension;
        extension.receiver = Some(Ty::obj_name(wk::kclass()));
        let mut concrete = original.clone();
        concrete.getter.is_abstract = false;
        let mut context = original;
        context.context_count = 1;
        for mut property in [wrong_type, extension, concrete, context] {
            realize_class_name_property(wk::kclass(), true, &mut property);
            assert_eq!(property.getter.compiler_intrinsic, None);
        }
    }
    let mut other_member = name_property("jClassName");
    realize_class_name_property(wk::kclass(), true, &mut other_member);
    assert_eq!(other_member.getter.compiler_intrinsic, None);
}
