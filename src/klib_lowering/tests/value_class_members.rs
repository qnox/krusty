//! A value class's members are emitted as members of its materialized class, reading the value
//! they are called on through the class's one field.

use super::*;

/// The provider record of the member `name` of the stdlib class `classifier` with `params`.
fn member(
    libraries: &KlibLibraries,
    classifier: &str,
    name: &str,
    params: &[Ty],
) -> crate::libraries::ExternalCallableRealization {
    let class = libraries
        .classifier(type_name(classifier))
        .expect("a stdlib class");
    let target = class.declared_callables[name]
        .functions()
        .iter()
        .find(|function| function.callable.params == params)
        .and_then(|function| function.callable.external_identity)
        .expect("an exact member declaration");
    libraries
        .external_callable(target)
        .expect("the provider realizes its own identity")
}

#[test]
fn a_value_class_member_is_a_member_of_its_class_reading_the_value_through_its_field() {
    let Some(root) = distribution_root() else {
        return;
    };
    let libraries = stdlib(&root);
    let selected = member(&libraries, "kotlin/time/Duration", "isNegative", &[]);
    let callable = selected.klib_body_callable().expect("a KLIB member");
    let facts = KlibCalleeFacts::with_declarations([callable], []);
    let mut unit = DependencyBodyUnit::default();
    let function = unit
        .lower_function(callable, libraries.declaration_bodies(), &facts)
        .unwrap_or_else(|decline| panic!("`Duration.isNegative`: {decline}"));
    let ir = unit.ir();
    let declared = &ir.functions[function as usize];
    assert_eq!(
        (
            declared.params.as_slice(),
            declared.ret,
            declared.is_static,
            declared.dispatch_receiver
        ),
        (
            &[][..],
            Ty::Boolean,
            false,
            Some(type_name("kotlin/time/Duration"))
        )
    );
    let [class] = &ir.classes[..] else {
        panic!("the unit materializes the value class alone");
    };
    assert_eq!(
        (class.fq_name, class.is_value, class.fields.len()),
        (type_name("kotlin/time/Duration"), true, 1)
    );
    assert_eq!(
        lowered_body(&unit, function),
        "scope {scope {return@Some(0) Lt(call v0!: kotlin/time/Duration.<get-rawValue>(): Long, \
         ImplicitCoercion<Long>(Int(0): Int): Long): Boolean: Nothing}: Nothing}"
    );
    // The private getter no provider records is emitted as its class's other members are.
    let [getter] = (0..ir.functions.len() as FunId)
        .filter(|&id| id != function)
        .collect::<Vec<_>>()[..]
    else {
        panic!("the unit emits the member and the getter it calls alone");
    };
    // Only the member is a function callers outside the unit can name.
    assert_eq!(
        unit.callable_functions()
            .map(|(_, function)| function)
            .collect::<Vec<_>>(),
        [function]
    );
    let declared = &ir.functions[getter as usize];
    assert_eq!(
        (
            declared.params.as_slice(),
            declared.ret,
            declared.is_static,
            declared.dispatch_receiver
        ),
        (
            &[][..],
            Ty::Long,
            false,
            Some(type_name("kotlin/time/Duration"))
        )
    );
    assert_eq!(
        lowered_body(&unit, getter),
        "scope {scope {return@Some(0) v0!: kotlin/time/Duration.rawValue: Long: Nothing}: Nothing}"
    );
}
