use super::*;

/// `when (x: Any) { 1 -> … }` is a boxed comparison, and the selected owned `rangeTo`
/// authorizes the counted range arm without relying on platform declarations.
#[test]
fn a_reference_when_subject_accepts_primitive_and_string_comparands() {
    ok("class OwnedIntRange\n\
        operator fun Int.rangeTo(other: Int): OwnedIntRange = OwnedIntRange()\n\
        operator fun OwnedIntRange.contains(value: Any): Boolean = true\n\
        fun f(x: Any): String = when (x) {\n\
            1 -> \"i\"\n\
            2L -> \"l\"\n\
            'c' -> \"c\"\n\
            \"s\" -> \"s\"\n\
            in 4..10 -> \"r\"\n\
            else -> \"o\"\n\
        }");
}

/// An unrelated comparand has no way to be equal, and a range whose boxed element the value can
/// never hold stays rejected after the exact owned range declaration is selected.
#[test]
fn an_unrelated_comparand_is_still_not_comparable_to_the_subject() {
    let (errors, _) = check("fun f(x: String): String = when (x) { 1 -> \"i\"; else -> \"o\" }");
    assert_eq!(
        errors,
        ["when condition type 'Int' is not comparable to subject 'String'"]
    );

    let (errors, _) = check(
        "class OwnedIntRange\n\
         operator fun Int.rangeTo(other: Int): OwnedIntRange = OwnedIntRange()\n\
         operator fun OwnedIntRange.contains(value: Int): Boolean = true\n\
         fun g(x: String): Boolean = x in 4..10",
    );
    assert_eq!(
        errors,
        ["operator 'contains' cannot be applied to range 'OwnedIntRange' and 'String'"]
    );

    // The owned floating range accepts exactly `Double`, so an unrelated boxed subject cannot
    // enter through a widened `contains` overload.
    let (errors, _) = check(
        "class OwnedDoubleRange\n\
         operator fun Double.rangeTo(other: Double): OwnedDoubleRange = OwnedDoubleRange()\n\
         operator fun OwnedDoubleRange.contains(value: Double): Boolean = true\n\
         fun h(x: Any): Boolean = x in 1.0..2.0",
    );
    assert_eq!(
        errors,
        ["operator 'contains' cannot be applied to range 'OwnedDoubleRange' and 'Any'"]
    );
}

#[test]
fn floating_membership_without_a_selected_range_operator_fails_closed() {
    let (errors, _) = check("fun f(x: Double): Boolean = x in 1.0..2.0");
    assert_eq!(
        errors,
        ["operator 'rangeTo' cannot be applied to 'Double' and 'Double'"]
    );
}

#[test]
fn reference_range_in_records_operator_calls_for_lowering() {
    let mut diagnostics = DiagSink::new();
    let source = "class VR { operator fun contains(v: V): Boolean = true }\n\
         class V { operator fun rangeTo(o: V): VR = VR() }\n\
         fun box(): Boolean = V() in V()..V()";
    let (file, _, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let info = info.expect("production retained analysis must check the source");
    assert_no_diags(&diagnostics);

    let in_range = file
        .expr_arena
        .iter()
        .enumerate()
        .find_map(|(index, expression)| match expression {
            Expr::InRange { start, .. } if info.ty(*start) == Ty::obj("V") => {
                Some(ExprId(index as u32))
            }
            _ => None,
        })
        .expect("source should contain reference in-range expression");

    assert!(matches!(
        info.resolved_operator_call(in_range, "rangeTo"),
        Some(ResolvedCall::Member(member))
            if matches!(member.origin, Origin::Module { .. })
                && member.member.owner.is_some_and(|owner| owner.matches("V"))
                && member.member.name == "rangeTo"
                && member.member.params.as_slice() == [Ty::obj("V")]
                && member.ret == Ty::obj("VR")
    ));
    assert!(matches!(
        info.resolved_operator_call(in_range, "contains"),
        Some(ResolvedCall::Member(member))
            if matches!(member.origin, Origin::Module { .. })
                && member.member.owner.is_some_and(|owner| owner.matches("VR"))
                && member.member.name == "contains"
                && member.member.params.as_slice() == [Ty::obj("V")]
                && member.ret == Ty::Boolean
    ));
}
