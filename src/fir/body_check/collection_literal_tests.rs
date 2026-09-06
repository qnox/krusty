use super::test_support::{
    checked_function_body, checked_function_body_with_platform, jvm_stdlib_semantics,
    root_expression,
};
use super::*;

#[test]
fn custom_collection_literal_keeps_the_selected_companion_operator() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CollectionLiterals\n\
         class MyList(val data: Array<out String>) {\n\
             companion object { operator fun of(vararg values: String) = MyList(values) }\n\
         }\n\
         fun make(): MyList = [\"O\", \"K\"]\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("collection literal");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("a custom collection literal must retain its checked factory call")
    };
    let target = call.target.module().expect("source companion operator");
    assert_eq!(index.callable_name(target), Some("of"));
    assert!(call.extension_receiver.is_none());
    assert_eq!(root.ty.get(), Ty::obj("MyList"));
}

#[test]
fn empty_collection_literal_keeps_the_selected_companion_block_operator() {
    let (body, index) = checked_function_body(
        "// LANGUAGE: +CompanionBlocks +CompanionExtensions +CollectionLiterals\n\
         class MyList {\n\
             companion { operator fun of(vararg values: String): MyList = MyList() }\n\
         }\n\
         fun make(): MyList = []\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("collection literal");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("a companion-block collection literal must retain its checked factory call")
    };
    let target = call
        .target
        .module()
        .expect("source companion-block operator");
    assert_eq!(index.callable_name(target), Some("of"));
    assert!(call.dispatch_receiver.is_none());
    assert!(call.extension_receiver.is_none());
    assert!(matches!(
        call.arguments.as_ref(),
        [FirCallArgument::Vararg { elements, .. }] if elements.is_empty()
    ));
    assert_eq!(root.ty.get(), Ty::obj("MyList"));
}

#[test]
fn standard_sequence_literal_keeps_external_factory_and_contextual_element_type() {
    let (body, _) = checked_function_body_with_platform(
        "// LANGUAGE: +CollectionLiterals\n\
         // WITH_STDLIB\n\
         fun make(): Sequence<Long> = [1, 2, 3]\n",
        "make",
        jvm_stdlib_semantics(),
    );
    let root = body.expr(root_expression(&body)).expect("sequence literal");
    let FirExprKind::Call(call) = &root.kind else {
        panic!("a standard collection literal must retain its selected external factory")
    };
    assert!(matches!(call.target, FirCallTarget::External { .. }));
    assert_eq!(
        root.ty.get(),
        Ty::obj_args("kotlin/sequences/Sequence", &[Ty::Long])
    );
    assert_eq!(call.substitutions.len(), 1);
    assert_eq!(call.substitutions[0].value.get(), Ty::Long);
    let elements = call
        .arguments
        .iter()
        .flat_map(|argument| match argument {
            FirCallArgument::Vararg { elements, .. } => elements.iter(),
            FirCallArgument::Expression { .. } | FirCallArgument::Default { .. } => {
                panic!("sequenceOf must retain the selected vararg mapping")
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(elements.len(), 3);
    assert!(elements.into_iter().all(|element| body
        .expr(element.value)
        .is_some_and(|value| value.ty.get() == Ty::Long)));
}

#[test]
fn unsigned_array_literal_is_checked_array_fir_not_a_value_class_call() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +CollectionLiterals\n\
         fun make(): UByteArray = [1u, 2u]\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("unsigned array literal");
    let FirExprKind::ArrayLiteral {
        array_type,
        elements,
    } = &root.kind
    else {
        panic!("an unsigned array literal must be checked array FIR")
    };
    assert_eq!(array_type.get(), Ty::obj("kotlin/UByteArray"));
    assert_eq!(elements.len(), 2);
    assert!(elements.iter().all(|element| {
        body.expr(element.value)
            .is_some_and(|value| value.ty.get() == Ty::UByte)
    }));
}

#[test]
fn array_of_unsigned_values_retains_the_semantic_reference_array_type() {
    let (body, _) = checked_function_body(
        "fun make(): Array<UInt> = arrayOf(13u, 4294967295u)\n",
        "make",
    );
    let root = body.expr(root_expression(&body)).expect("arrayOf call");
    let FirExprKind::ArrayLiteral {
        array_type,
        elements,
    } = &root.kind
    else {
        panic!("arrayOf must be represented as checked array FIR")
    };
    assert_eq!(array_type.get(), Ty::obj_args("kotlin/Array", &[Ty::UInt]));
    assert_eq!(elements.len(), 2);
    assert!(elements.iter().all(|element| {
        body.expr(element.value)
            .is_some_and(|value| value.ty.get() == Ty::UInt)
    }));
}

#[test]
fn inherited_size_lookup_does_not_replace_unsigned_array_factory_type() {
    let (body, _) = checked_function_body("fun make(): Int = uintArrayOf(1u).size\n", "make");
    let array = (0..body.expression_count())
        .filter_map(|raw| body.expr(FirExprId::from_raw(raw as u32)))
        .find(|expression| matches!(expression.kind, FirExprKind::ArrayLiteral { .. }))
        .expect("unsigned array factory FIR");
    assert_eq!(array.ty.get(), Ty::obj("kotlin/UIntArray"));
    let FirExprKind::ArrayLiteral { array_type, .. } = &array.kind else {
        unreachable!()
    };
    assert_eq!(array_type.get(), Ty::obj("kotlin/UIntArray"));
}

#[test]
fn spread_collection_literal_uses_the_vararg_array_expectation() {
    let (body, _) = checked_function_body(
        "// LANGUAGE: +CollectionLiterals\n\
         fun make(): Array<String> = arrayOf(*[\"O\"], *arrayOf(elements = arrayOf(\"K\")))\n",
        "make",
    );
    let root = body
        .expr(root_expression(&body))
        .expect("outer array literal");
    let FirExprKind::ArrayLiteral {
        array_type,
        elements,
    } = &root.kind
    else {
        panic!("arrayOf must retain checked array FIR")
    };
    assert_eq!(
        array_type.get(),
        Ty::obj_args("kotlin/Array", &[Ty::String])
    );
    assert_eq!(elements.len(), 2);
    assert!(elements[0].spread);
    assert_eq!(
        body.expr(elements[0].value)
            .expect("spread collection literal")
            .ty
            .get(),
        Ty::obj_args("kotlin/Array", &[Ty::String]),
    );
    assert!(elements[1].spread);
    let nested = body
        .expr(elements[1].value)
        .expect("nested arrayOf with named whole-array argument");
    let FirExprKind::ArrayLiteral { elements, .. } = &nested.kind else {
        panic!("nested arrayOf must retain checked array FIR")
    };
    assert_eq!(elements.len(), 1);
    assert!(elements[0].spread);
}
