use super::*;

#[test]
fn selected_iteration_properties_include_every_lowered_member_read() {
    let property = |raw| ExternalPropertyId::from_raw(raw);
    let mut plans = IterationPlans::default();
    plans.members.insert(
        Ty::Int,
        ProgressionPlan {
            class: wk::ProgressionClass::Range,
            first: LoopMemberProperty {
                property: property(1),
                ty: Ty::Int,
            },
            last: LoopMemberProperty {
                property: property(2),
                ty: Ty::Int,
            },
            step: LoopMemberProperty {
                property: property(3),
                ty: Ty::Int,
            },
            compare: None,
        },
    );
    plans.indexed_value = Some(IndexedValueProperties {
        index: property(4),
        value: property(5),
    });
    plans.indices.insert(
        ExprId::from_raw(0),
        IndicesSize {
            view: Ty::Int,
            size: LoopMemberProperty {
                property: property(6),
                ty: Ty::Int,
            },
        },
    );

    let selected = plans.selected_member_properties();
    assert_eq!(selected.len(), 6);
    for raw in 1..=6 {
        assert!(selected.contains(&property(raw)), "missing property {raw}");
    }
}
