use super::*;

#[test]
fn foreach_with_callable_reference_keeps_the_selected_external_call_without_splice_plan() {
    let (body, _) = checked_function_body_with_platform(
        "class Item\n\
         class Collector { fun add(item: Item): Collector = this }\n\
         fun read(collector: Collector): Unit { listOf(Item()).forEach(collector::add) }\n",
        "read",
        jvm_semantics(),
    );

    let foreach = (0..body.expression_count()).find_map(|raw| {
        let expression = body.expr(FirExprId::from_raw(raw as u32))?;
        let FirExprKind::Call(call) = &expression.kind else {
            return None;
        };
        let FirCallTarget::External { inline_plan, .. } = &call.target else {
            return None;
        };
        let reference = call.arguments.iter().find_map(|argument| match argument {
            FirCallArgument::Expression { value, .. }
                if matches!(
                    body.expr(*value).map(|expression| &expression.kind),
                    Some(FirExprKind::CallableReference { .. })
                ) =>
            {
                Some(value)
            }
            _ => None,
        })?;
        Some((inline_plan, reference))
    });
    let Some((inline_plan, _)) = foreach else {
        panic!("forEach must retain its checked callable-reference argument")
    };
    assert!(
        inline_plan.is_none(),
        "a callable-reference value has no lambda body to splice"
    );
}

#[test]
fn ordinary_map_literal_keeps_the_selected_external_call_without_splice_plan() {
    let (body, _) = checked_function_body_with_platform(
        "fun read(values: List<Int>): List<Int> = values.map { it + 1 }\n",
        "read",
        jvm_stdlib_semantics(),
    );

    let map = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
            return None;
        };
        let FirCallTarget::External { inline_plan, .. } = &call.target else {
            return None;
        };
        call.arguments
            .iter()
            .any(|argument| {
                matches!(
                    argument,
                    FirCallArgument::Expression { value, .. }
                        if matches!(
                            body.expr(*value).map(|expression| &expression.kind),
                            Some(FirExprKind::Lambda { .. })
                        )
                )
            })
            .then_some(inline_plan)
    });
    assert!(matches!(map, Some(None)));
}

#[test]
fn suspending_map_publishes_a_complete_provider_owned_collection_plan() {
    let (body, _) = checked_function_body_with_platform(
        "operator fun <K, V> Map<K, V>.iterator(): Iterator<Map.Entry<K, V>> =\n\
             emptyList<Map.Entry<K, V>>().iterator()\n\
         suspend fun render(entry: Map.Entry<String, Int>): String = entry.key\n\
         suspend fun collect(values: Map<String, Int>): List<String> =\n\
             values.map { render(it) }\n",
        "collect",
        jvm_stdlib_semantics(),
    );

    let plan = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
            return None;
        };
        let FirCallTarget::External {
            inline_plan: Some(plan),
            ..
        } = &call.target
        else {
            return None;
        };
        matches!(plan.as_ref(), FirInlineBodyPlan::CollectionTransform { .. })
            .then_some(plan.as_ref())
    });
    let Some(FirInlineBodyPlan::CollectionTransform {
        lambda_parameter,
        traversal,
        factory,
        capacity,
        append,
        accumulator,
        ..
    }) = plan
    else {
        panic!("selected map call must publish its complete checked structural plan")
    };
    assert_eq!(*lambda_parameter, 0);
    let crate::fir::FirInlineIterationTraversal::Iterator {
        prepare,
        has_next,
        next,
    } = traversal
    else {
        panic!("map must carry its declaration-owned iterator traversal")
    };
    let [entries, iterator] = prepare.as_ref() else {
        panic!("Map.map must carry its entries and iterator calls")
    };
    assert_eq!(
        entries.receiver.get(),
        Ty::obj_args(
            "kotlin/collections/Map",
            &[Ty::out_projection(Ty::String), Ty::Int],
        )
    );
    assert_eq!(entries.parameters.as_ref(), []);
    assert_eq!(iterator.receiver, entries.result);
    assert_eq!(iterator.parameters.as_ref(), []);
    assert_eq!(has_next.receiver, iterator.result);
    assert_eq!(has_next.parameters.as_ref(), []);
    assert_eq!(has_next.result.get(), Ty::Boolean);
    assert_eq!(next.receiver, iterator.result);
    assert_eq!(next.parameters.as_ref(), []);
    assert_eq!(
        next.result.get(),
        Ty::obj_args("kotlin/collections/Map$Entry", &[Ty::String, Ty::Int])
    );
    let crate::fir::FirInlineCollectionCapacity::Member(capacity) =
        capacity.as_ref().expect("Map.map capacity")
    else {
        panic!("Map.map must use its structurally decoded member capacity")
    };
    let crate::fir::FirInlineCollectionAppend::Member(append) = append.as_ref() else {
        panic!("Map.map must use its structurally decoded member append")
    };
    let identities = [
        entries.declaration,
        iterator.declaration,
        has_next.declaration,
        next.declaration,
        *factory,
        capacity.declaration,
        append.declaration,
    ];
    assert_eq!(
        identities
            .iter()
            .copied()
            .collect::<crate::fx_hash::HashSet<_>>()
            .len(),
        identities.len(),
        "entries/iterator/hasNext/next/factory/capacity/append remain exact declarations"
    );
    assert_eq!(
        accumulator.get(),
        Ty::obj_args("java/util/ArrayList", &[Ty::String])
    );
    assert_eq!(append.parameters[0].get(), Ty::String);
}

#[test]
fn suspending_flat_map_specializes_every_extension_append_parameter() {
    let (body, _) = checked_function_body_with_platform(
        "suspend fun expand(value: Int): List<String> = listOf(value.toString())\n\
         suspend fun collect(values: List<Int>): List<String> =\n\
             values.flatMap { expand(it) }\n",
        "collect",
        jvm_stdlib_semantics(),
    );

    let plan = (0..body.expression_count()).find_map(|raw| {
        let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
            return None;
        };
        let FirCallTarget::External {
            inline_plan: Some(plan),
            ..
        } = &call.target
        else {
            return None;
        };
        matches!(plan.as_ref(), FirInlineBodyPlan::CollectionTransform { .. })
            .then_some(plan.as_ref())
    });
    let Some(FirInlineBodyPlan::CollectionTransform {
        capacity,
        append,
        accumulator,
        ..
    }) = plan
    else {
        panic!("selected flatMap call must publish its exact extension append contract")
    };
    let crate::fir::FirInlineCollectionAppend::Extension(append) = append.as_ref() else {
        panic!("selected flatMap call must publish its exact extension append contract")
    };
    assert!(capacity.is_none());
    assert_eq!(
        accumulator.get(),
        Ty::obj_args("java/util/ArrayList", &[Ty::String])
    );
    assert_eq!(append.parameters.len(), 2);
    assert_eq!(append.source_receiver, append.parameters[0]);
    assert_eq!(
        append.parameters[0].get(),
        Ty::obj_args(
            "kotlin/collections/MutableCollection",
            &[Ty::in_projection(Ty::String)],
        )
    );
    assert_eq!(
        append.parameters[1].get(),
        Ty::obj_args("kotlin/collections/Iterable", &[Ty::String])
    );
    assert_eq!(append.result.get(), Ty::Boolean);
}

#[test]
fn safe_map_publishes_one_complete_receiver_specialized_collection_plan() {
    let (body, _) = checked_function_body_with_platform(
        "class Holder(val callback: suspend () -> String)\n\
         suspend fun collect(values: List<Int>?, holder: Holder?): List<String>? =\n\
             values?.map { holder?.callback() ?: \"none\" }\n",
        "collect",
        jvm_stdlib_semantics(),
    );

    let plans = (0..body.expression_count())
        .filter_map(|raw| {
            let FirExprKind::Call(call) = &body.expr(FirExprId::from_raw(raw as u32))?.kind else {
                return None;
            };
            let FirCallTarget::External {
                inline_plan: Some(plan),
                ..
            } = &call.target
            else {
                return None;
            };
            matches!(plan.as_ref(), FirInlineBodyPlan::CollectionTransform { .. })
                .then_some(plan.as_ref())
        })
        .collect::<Vec<_>>();
    let [FirInlineBodyPlan::CollectionTransform { traversal, .. }] = plans.as_slice() else {
        panic!("the safe map must publish exactly one collection plan: {plans:?}")
    };
    let crate::fir::FirInlineIterationTraversal::Iterator {
        prepare,
        has_next,
        next,
    } = traversal
    else {
        panic!("the safe map must retain the provider's iterator traversal")
    };
    assert_eq!(prepare.len(), 1);
    assert_ne!(prepare[0].declaration, has_next.declaration);
    assert_ne!(has_next.declaration, next.declaration);
}
