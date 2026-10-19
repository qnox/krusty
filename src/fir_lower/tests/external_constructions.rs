//! Dependency constructions keep the selected declaration's parameters beside their specialization.

use super::*;

/// `AtomicReference<V>(V)` selected as `AtomicReference(Money(7))`: the construction's operand is
/// the specialized platform `Money!`, while the declaration destination stays the class formal `V`. A target
/// that represents values across erasure reads the second, not the first.
#[test]
fn a_java_generic_constructor_records_its_declared_type_parameter_destination() {
    let ir = lower_single_source_with_jvm_stdlib(
        "@JvmInline value class Money(val cents: Int)\n\
         fun make() = java.util.concurrent.atomic.AtomicReference(Money(7))\n",
        "ExternalGenericConstruction",
    );
    let constructions = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| match expression {
            IrExpr::New {
                internal,
                args,
                ctor_params,
                external_target: Some(_),
                ..
            } if internal.matches("java/util/concurrent/atomic/AtomicReference") => {
                Some((id as crate::ir::ExprId, args, ctor_params))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(construction, args, ctor_params)] = constructions.as_slice() else {
        panic!("one external construction, got {constructions:?}")
    };
    let [argument] = args.as_slice() else {
        panic!("one constructor operand, got {args:?}")
    };
    assert_eq!(ir.checked_type(*argument), Some(Ty::obj("Money")));
    assert_eq!(
        ctor_params.as_deref(),
        Some([Ty::platform_nullable(Ty::obj("Money"))].as_slice())
    );
    let declared = ir.construction_declared_params[construction].as_ref();
    // Java's unannotated class formal `V` is the platform `V!` with Kotlin's implicit `Any?` bound.
    let formal = Ty::platform_nullable(Ty::ty_param("V", Ty::nullable(Ty::obj("kotlin/Any"))));
    assert_eq!(declared, [formal]);
}

/// A super-constructor delegation to the same Java constructor keeps the class formal as the
/// operand destination, while its argument mapping used the specialized `Money!`.
#[test]
fn a_java_generic_super_delegation_records_its_declared_type_parameter_destination() {
    let ir = lower_single_source_with_jvm_stdlib(
        "@JvmInline value class Money(val cents: Int)\n\
         class Primary(value: Money) : java.util.concurrent.atomic.AtomicReference<Money>(value)\n\
         class Secondary : java.util.concurrent.atomic.AtomicReference<Money> {\n\
             constructor(value: Money) : super(value)\n\
         }\n",
        "ExternalGenericSuperDelegation",
    );
    let formal = Ty::platform_nullable(Ty::ty_param("V", Ty::nullable(Ty::obj("kotlin/Any"))));
    let class = |name: &str| {
        ir.classes
            .iter()
            .find(|class| class.fq_name.matches(name))
            .unwrap_or_else(|| panic!("class {name}"))
    };
    let primary = class("Primary");
    assert!(primary
        .superclass
        .matches("java/util/concurrent/atomic/AtomicReference"));
    assert_eq!(primary.super_ctor_params, [formal]);
    let [argument] = primary.super_args.as_slice() else {
        panic!("one superclass operand, got {:?}", primary.super_args)
    };
    assert_eq!(ir.checked_type(*argument), Some(Ty::obj("Money")));
    let secondary = class("Secondary");
    let [constructor] = secondary.secondary_ctors.as_slice() else {
        panic!("one secondary constructor")
    };
    let crate::ir::CtorDelegateTarget::Super { target_params, .. } = &constructor.delegate else {
        panic!("a super(...) delegation, got {:?}", constructor.delegate)
    };
    assert_eq!(target_params.as_slice(), [formal]);
}
