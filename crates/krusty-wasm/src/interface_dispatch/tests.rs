use super::*;
use crate::encode::{CompositeType, SubType};
use crate::objects::immutable;
use krusty::types::type_name;

fn signature() -> (Vec<ValType>, Vec<ValType>) {
    (vec![REFERENCE], vec![REFERENCE])
}

#[test]
fn a_published_member_cannot_dispatch_to_one_implementation_when_another_nominal_instance_is_missing(
) {
    let mut module = Module::default();
    let mut interfaces = InterfaceDispatch::default();
    let owner = type_name("producer/Root");
    let member = interfaces
        .numbered(&mut module, "producer", 0, signature())
        .unwrap();
    interfaces.publish(vec![1], owner, member).unwrap();
    let itable = module.ty(CompositeType::Array(immutable(ValType::Ref {
        nullable: true,
        heap: HeapType::Func,
    })));
    let first_structure = module.subtype(SubType {
        composite: CompositeType::Struct(Vec::new()),
        supertype: None,
        open: false,
    });
    let second_structure = module.subtype(SubType {
        composite: CompositeType::Struct(Vec::new()),
        supertype: None,
        open: false,
    });
    let (first, _) = interfaces.class(&mut module, itable, vec![first_structure]);
    let (second, _) = interfaces.class(&mut module, itable, vec![second_structure]);
    interfaces.instance_of(first, owner);
    interfaces.instance_of(second, owner);
    let first_body = module.declare();
    interfaces.implement(first, member, first_body).unwrap();
    interfaces.stub(&mut module, member);
    assert_eq!(
        interfaces.declined(),
        Some("a call of a `producer/Root` member on an instance that does not implement it".into())
    );
    // Installing the distinct SAM's actual member makes the call complete; it cannot use the
    // first implementor's body merely because that had been the only function recorded.
    let second_body = module.declare();
    interfaces.implement(second, member, second_body).unwrap();
    assert_eq!(interfaces.declined(), None);
    assert!(!matches!(
        interfaces.shape(&interfaces.members[member.0 as usize]),
        Some(Shape::Direct(_))
    ));
}

#[test]
fn published_dependency_identity_joins_preserve_owner_and_complete_prototype_without_mutation_on_error(
) {
    let mut module = Module::default();
    let mut interfaces = InterfaceDispatch::default();
    let owner = type_name("producer/Root");
    let wrong = type_name("unrelated/Root");
    let member = interfaces
        .numbered(&mut module, "producer", 3, signature())
        .unwrap();
    interfaces.publish(vec![1], owner, member).unwrap();
    assert_eq!(
        interfaces.library(&mut module, wrong, &[1], signature()),
        Err("one interface identity has two declaring owners".into())
    );
    assert_eq!(
        interfaces.library(
            &mut module,
            owner,
            &[1],
            (vec![REFERENCE, ValType::I32], vec![REFERENCE])
        ),
        Err("a library member carried two ways".into())
    );
    assert_eq!(
        interfaces.library(
            &mut module,
            owner,
            &[1],
            (vec![REFERENCE], vec![ValType::I64])
        ),
        Err("a library member carried two ways".into())
    );
    assert_eq!(
        interfaces.library(&mut module, owner, &[1], signature()),
        Ok(member)
    );
    // Checked inherited declarations may share one numbered member, but their own identities
    // retain distinct nominal owners and both completeness obligations.
    interfaces.publish(vec![2], wrong, member).unwrap();
    assert_eq!(
        interfaces.library(&mut module, wrong, &[2], signature()),
        Ok(member)
    );
    assert_eq!(
        interfaces.members[member.0 as usize].declaring_owners,
        vec![owner, wrong]
    );
    let other = interfaces
        .numbered(&mut module, "consumer", 3, signature())
        .unwrap();
    assert_eq!(
        interfaces.publish(vec![1], owner, other),
        Err("two interface declarations published one identity".into())
    );
    assert_eq!(interfaces.identified(&[1]), Some(member));
}

#[test]
fn callable_protocols_intern_exact_erased_signatures_by_arity() {
    let mut module = Module::default();
    let mut dispatch = InterfaceDispatch::default();
    let one = dispatch.function_invoke(&mut module, 1);
    assert_eq!(one, dispatch.function_invoke(&mut module, 1));
    assert_ne!(one, dispatch.function_invoke(&mut module, 0));
    assert_eq!(
        module.func_signature(dispatch.ty(one)),
        Some((vec![REFERENCE; 2], vec![REFERENCE]))
    );
}

#[test]
fn advertised_callable_membership_does_not_depend_on_emitting_its_body() {
    let mut module = Module::default();
    let mut dispatch = InterfaceDispatch::default();
    let (class, _) = dispatch.class(&mut module, 0, vec![1]);
    dispatch.function_instance_of(class, 1, false);
    let member = dispatch.function_invoke(&mut module, 1);
    let test = dispatch.function_test(&mut module, 1);
    assert_eq!(test, dispatch.function_test(&mut module, 1));
    assert_eq!(dispatch.classes[class.0 as usize].function_arities, vec![1]);
    assert_eq!(dispatch.declined(), None);
    dispatch.stub(&mut module, member);
    assert_eq!(
        dispatch.declined(),
        Some("a callable of arity 1 with no emitted invocation".into())
    );
    let function = module.declare();
    dispatch.implement(class, member, function).unwrap();
    assert_eq!(dispatch.declined(), None);
}

#[test]
fn a_representation_implements_a_member_for_every_class_below_its_struct() {
    let mut module = Module::default();
    let mut interfaces = InterfaceDispatch::default();
    let owner = type_name("producer/Root");
    let member = interfaces
        .numbered(&mut module, "producer", 0, signature())
        .unwrap();
    interfaces.publish(vec![1], owner, member).unwrap();
    let itable = module.ty(CompositeType::Array(immutable(ValType::Ref {
        nullable: true,
        heap: HeapType::Func,
    })));
    let base = module.subtype(SubType {
        composite: CompositeType::Struct(Vec::new()),
        supertype: None,
        open: true,
    });
    let derived = module.subtype(SubType {
        composite: CompositeType::Struct(Vec::new()),
        supertype: Some(base),
        open: false,
    });
    let (class, _) = interfaces.class(&mut module, itable, vec![derived, base]);
    interfaces.instance_of(class, owner);
    interfaces.stub(&mut module, member);
    assert_eq!(
        interfaces.declined(),
        Some("a call of a `producer/Root` member on an instance that does not implement it".into())
    );
    let body = module.declare();
    interfaces
        .implement_representation(member, base, body)
        .unwrap();
    assert_eq!(interfaces.declined(), None);
}
