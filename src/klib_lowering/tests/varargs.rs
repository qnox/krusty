//! Declaration-owned varargs keep one packed array in the common callable ABI.

use super::super::function_lowering::{raw_function_header, HeaderPurpose};
use super::demo_package::{declaration, T};
use super::demo_package::{demo, source_body};
use super::*;
use crate::metadata::klib_ir::tree::KlibIrVarargElement;

#[test]
fn a_declared_vararg_authenticates_its_packed_array_and_element() {
    let mut declaration = declaration(("b", &[T::Int], T::Int));
    declaration.params = vec![kotlin_class("IntArray")];
    declaration.vararg = Some(0);
    let libraries = KlibLibraries::from_named_packages(
        "demo",
        vec![(
            vec!["demo".into()],
            KotlinPackage {
                functions: vec![declaration],
                ..Default::default()
            },
        )],
    )
    .unwrap();
    let frozen = frozen_function(&libraries, "demo", "b", None, &[Ty::array(Ty::Int)]);
    let selected = frozen.klib_body_callable().unwrap();
    let mut arena = KlibIrArena::default();
    let int = arena.push_type(T::Int.klib());
    let string = arena.push_type(T::String.klib());
    let array_signature = metadata_class_signature(
        MetadataContainer {
            package: &["kotlin".into()],
            classes: &[],
            native_interop_library: false,
        },
        MetadataClass {
            name: "IntArray",
            type_params: &[],
            expect: false,
        },
    );
    let array = arena.push_type(KlibIrType::Simple {
        classifier: public_symbol(KlibIrSymbolKind::Class, array_signature),
        nullability: KlibIrNullability::NotSpecified,
        arguments: vec![],
        annotations: vec![],
        abbreviation: None,
    });
    let KlibDeclarationSignature::Public(signature) = selected.signature() else {
        panic!("function")
    };
    let function = KlibIrFunction {
        base: base(public_symbol(KlibIrSymbolKind::Function, signature.clone())),
        name: "b".into(),
        constructor: false,
        type_parameters: vec![],
        dispatch_receiver: None,
        context_parameters: vec![],
        extension_receiver: None,
        regular_parameters: vec![KlibIrParameter {
            base: base(local_symbol(KlibIrSymbolKind::ValueParameter, 1)),
            name: "a".into(),
            ty: array,
            vararg_element_type: Some(int),
            default_value: None,
        }],
        return_type: int,
        body: None,
        overridden: vec![],
        companion_extension_class: None,
        prepared_inline_file: None,
    };
    let header = raw_function_header(Some(selected), HeaderPurpose::External, &arena, &function)
        .expect("the real declaration's vararg marker authenticates without expanding its ABI");
    assert_eq!(header.params(), &[Ty::array(Ty::Int)]);
    assert_eq!(selected.vararg_index(), Some(0));
    let mut unnamed = frozen.clone();
    unnamed.parameter_identities[0] = crate::fir::ResolvedParameterIdentity::Unnamed { ordinal: 0 };
    super::super::function_lowering::prepared_inline_header(
        unnamed.klib_body_callable().unwrap(),
        &[],
        &arena,
        &function,
    )
    .expect("an unnamed ordinary value parameter remains a valid declared vararg");
    let mut wrong_element = function.clone();
    wrong_element.regular_parameters[0].vararg_element_type = Some(string);
    assert_eq!(
        raw_function_header(
            Some(selected),
            HeaderPurpose::External,
            &arena,
            &wrong_element
        )
        .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "the serialized vararg element disagrees with its packed array".into()
        )),
    );
    let mut malformed_array = arena.ty(array).clone();
    let KlibIrType::Simple { arguments, .. } = &mut malformed_array else {
        panic!("array")
    };
    arguments.push(crate::metadata::klib_ir::tree::KlibIrTypeArgument::Type {
        variance: crate::metadata::klib_ir::tree::KlibIrVariance::Invariant,
        ty: string,
    });
    let malformed_array = arena.push_type(malformed_array);
    let mut malformed = function.clone();
    malformed.regular_parameters[0].ty = malformed_array;
    assert_eq!(
        raw_function_header(None, HeaderPurpose::External, &arena, &malformed).err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "a vararg parameter is not a non-null packed array".into()
        )),
    );
    let mut missing_marker = function;
    missing_marker.regular_parameters[0].vararg_element_type = None;
    assert_eq!(
        raw_function_header(
            Some(selected),
            HeaderPurpose::External,
            &arena,
            &missing_marker
        )
        .err(),
        Some(KlibBodyDeclineReason::SignatureMismatch(
            "the serialized vararg position disagrees with its selected declaration".into()
        )),
    );
}

#[test]
fn vararg_position_survives_context_and_extension_parameter_coordinates() {
    let mut declaration = declaration(("b", &[T::Int, T::Int], T::Int));
    declaration.params[1] = kotlin_class("IntArray");
    declaration.context_count = 1;
    declaration.context_kinds = vec![crate::types::ContextParameterKind::Named];
    declaration.receiver = Some(kotlin_class("String"));
    declaration.vararg = Some(1);
    let libraries = KlibLibraries::from_named_packages(
        "demo",
        vec![(
            vec!["demo".into()],
            KotlinPackage {
                functions: vec![declaration],
                ..Default::default()
            },
        )],
    )
    .unwrap();
    let frozen = frozen_function(
        &libraries,
        "demo",
        "b",
        Some(Ty::String),
        &[Ty::Int, Ty::array(Ty::Int)],
    );
    assert_eq!(frozen.params, [Ty::Int, Ty::String, Ty::array(Ty::Int)]);
    let target = libraries
        .symbols(SymbolNamespace::Package(type_name("demo")), "b")
        .callables
        .functions()[0]
        .callable
        .external_identity
        .unwrap();
    let mut provider = libraries.external_callable(target).unwrap();
    assert_eq!(
        provider.klib_body_callable().unwrap().vararg_index(),
        Some(2)
    );
    // These are specialization fields, not authority for a declaration's vararg slot.
    provider.callable.vararg_index = Some(0);
    provider.callable.vararg_elem = Some(Ty::Double);
    assert_eq!(
        provider.klib_body_callable().unwrap().vararg_index(),
        Some(2)
    );
    assert_eq!(frozen.klib_body_callable().unwrap().vararg_index(), Some(2));
}

#[test]
fn packed_varargs_preserve_nullable_elements_but_not_nullable_or_projected_array_shapes() {
    use super::super::function_lowering::vararg_parameters::array_element;
    let element = Ty::nullable(Ty::String);
    let reference = Ty::obj_args("kotlin/Array", &[Ty::out_projection(element)]);
    assert_eq!(array_element(reference), Ok(element));
    assert_eq!(array_element(Ty::array(Ty::Int)), Ok(Ty::Int));
    for invalid in [
        Ty::nullable(reference),
        Ty::Int,
        Ty::ty_param("array-bound-is-not-an-array", reference),
        Ty::obj("kotlin/Array"),
        Ty::obj_args("kotlin/Array", &[Ty::Int, Ty::String]),
        Ty::obj_args("kotlin/IntArray", &[Ty::String]),
    ] {
        assert_eq!(
            array_element(invalid),
            Err(KlibBodyDeclineReason::SignatureMismatch(
                "a vararg parameter is not a non-null packed array".into()
            ))
        );
    }
    for invalid in [Ty::in_projection(element), Ty::star_projection(element)] {
        assert_eq!(
            array_element(Ty::obj_args("kotlin/Array", &[invalid])),
            Err(KlibBodyDeclineReason::SignatureMismatch(
                "a vararg packed array has an incompatible element projection".into()
            ))
        );
    }
}

#[test]
fn serialized_varargs_preserve_one_array_and_each_spread_in_source_order() {
    let demo = demo(&[("a", &[T::IntArray], T::IntArray)], |_, body| {
        let one = body.int(1);
        let middle = body.param(0);
        let two = body.int(2);
        let element_type = body.ty(T::Int);
        let array = body.expr(
            T::IntArray,
            KlibIrExprKind::Vararg {
                element_type,
                elements: vec![
                    KlibIrVarargElement::Expression(one),
                    KlibIrVarargElement::Spread(middle),
                    KlibIrVarargElement::Expression(two),
                ],
            },
        );
        Some(vec![body.ret(array)])
    });
    let mut unit = DependencyBodyUnit::default();
    let function = demo
        .lower(&mut unit, "a", &[])
        .expect("a packed vararg lowers");
    validated(unit.ir());
    assert_eq!(
        lowered_body(&unit, function),
        source_body(
            "fun a(a: IntArray): IntArray { return intArrayOf(1, *a, 2) }",
            "a",
            1
        ),
    );
    let arrays = unit
        .ir()
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Vararg {
                array_type,
                spreads,
                elements,
            } => Some((*array_type, spreads.clone(), elements.len())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(arrays, [(Ty::array(Ty::Int), vec![false, true, false], 3)]);
}

#[test]
fn malformed_serialized_varargs_decline_without_publishing_partial_ir() {
    for (wrong_element, spread, expected) in [
        (
            true,
            false,
            KlibBodyDeclineReason::SignatureMismatch(
                "a serialized vararg expression disagrees with its packed array element".into(),
            ),
        ),
        (false, true, KlibBodyDeclineReason::ImplicitConversion),
    ] {
        let demo = demo(&[("a", &[], T::IntArray)], |_, body| {
            let value = body.int(1);
            let element_type = body.ty(if wrong_element { T::String } else { T::Int });
            let array = body.expr(
                T::IntArray,
                KlibIrExprKind::Vararg {
                    element_type,
                    elements: vec![if spread {
                        KlibIrVarargElement::Spread(value)
                    } else {
                        KlibIrVarargElement::Expression(value)
                    }],
                },
            );
            Some(vec![body.ret(array)])
        });
        let mut unit = DependencyBodyUnit::default();
        let decline = demo.lower(&mut unit, "a", &[]).unwrap_err();
        assert_eq!(decline.reason(), &expected);
        super::demo_package::assert_unit_is_empty(&unit);
    }
}
