use super::*;
use crate::jvm::jvm_libraries::generic_signatures::{
    parse_class_gsig_with, parse_field_gsig_with, parse_member_method_gsig_with,
};
use std::cell::RefCell;

#[test]
fn explicit_outer_arguments_reach_construction_before_flattening() {
    let seen = RefCell::new(Vec::new());
    let parsed = parse_type_with(
        "Lfixture/Outer<Ljava/lang/String;>.Inner<Ljava/lang/Integer;>;tail",
        true,
        &|segments| {
            seen.borrow_mut().push(segments.to_vec());
            ordinary_application(segments)
        },
    )
    .expect("complete nested syntax");
    assert_eq!(
        *seen.borrow(),
        [
            vec![ClassSegment {
                identity: type_name("java/lang/String"),
                arguments: None,
            }],
            vec![ClassSegment {
                identity: type_name("java/lang/Integer"),
                arguments: None,
            }],
            vec![
                ClassSegment {
                    identity: type_name("fixture/Outer"),
                    arguments: Some(vec![Ty::String]),
                },
                ClassSegment {
                    identity: type_name("fixture/Outer$Inner"),
                    arguments: Some(vec![Ty::obj("java/lang/Integer")]),
                },
            ],
        ]
    );
    assert_eq!(parsed.erasure.as_deref(), Some("Lfixture/Outer$Inner;"));
    assert_eq!(parsed.rest, "tail");
    assert!(
        parsed.field_inexact,
        "standalone reader still loses outer arguments"
    );
}

#[test]
fn omitted_arguments_are_distinct_from_written_arguments_at_each_segment() {
    let seen = RefCell::new(Vec::new());
    for signature in [
        "Lfixture/Outer.Inner;",
        "Lfixture/Outer<Ljava/lang/String;>.Inner;",
        "Lfixture/Outer.Inner<Ljava/lang/String;>;",
    ] {
        parse_type_with(signature, false, &|segments| {
            if segments.len() == 2 {
                seen.borrow_mut().push(
                    segments
                        .iter()
                        .map(|segment| segment.arguments.clone())
                        .collect::<Vec<_>>(),
                );
            }
            ordinary_application(segments)
        })
        .expect("class occurrence");
    }
    assert_eq!(
        *seen.borrow(),
        [
            vec![None, None],
            vec![Some(vec![Ty::String]), None],
            vec![None, Some(vec![Ty::String])],
        ]
    );
}

#[test]
fn physical_field_erasure_does_not_come_from_the_constructed_semantic_type() {
    let build = |_: &[ClassSegment]| {
        Some(ClassApplication {
            ty: Ty::obj_args("semantic/Constructed", &[Ty::String]),
            discarded_outer_arguments: false,
        })
    };
    let signature = "[Lphysical/Outer<Ljava/lang/String;>.Inner<TT;>;";
    let bounds = crate::fx_hash::HashMap::from_iter([("T".to_owned(), Ty::String)]);
    assert_eq!(
        parse_field_gsig_with(signature, "[Lphysical/Outer$Inner;", &bounds, &build),
        Some((
            Ty::array(Ty::obj_args("semantic/Constructed", &[Ty::String])),
            true
        )),
    );
    for descriptor in [
        "[Lsemantic/Constructed;",
        "Lphysical/Outer$Inner;",
        "[Lphysical/Outer$Inner;I",
    ] {
        assert_eq!(
            parse_field_gsig_with(signature, descriptor, &bounds, &build),
            None
        );
    }
}

#[test]
fn construction_covers_formal_bounds_method_positions_and_supertypes() {
    let seen = RefCell::new(Vec::new());
    let build = |segments: &[ClassSegment]| {
        seen.borrow_mut().push(segments.to_vec());
        Some(ClassApplication {
            ty: Ty::String,
            discarded_outer_arguments: false,
        })
    };
    let method = parse_member_method_gsig_with(
        "<T:Lbounds/Bound;>([Lparams/Parameter;)Lreturns/Result;",
        &crate::fx_hash::HashMap::default(),
        &build,
    )
    .expect("method");
    assert_eq!(
        method.formal_bounds,
        [vec![Ty::platform_nullable(Ty::String)]]
    );
    assert_eq!(method.params, [Ty::array(Ty::String)]);
    assert_eq!(method.ret, Ty::String);
    let (_, _, supers) =
        parse_class_gsig_with("Lsupers/Base;Lsupers/Interface;", &build).expect("class");
    assert_eq!(supers, [Ty::String, Ty::String]);
    assert_eq!(
        *seen.borrow(),
        [
            "bounds/Bound",
            "params/Parameter",
            "returns/Result",
            "supers/Base",
            "supers/Interface"
        ]
        .map(|name| vec![ClassSegment {
            identity: type_name(name),
            arguments: None
        }]),
    );
}

#[test]
fn a_constructor_refusal_cannot_retry_the_standalone_interpretation() {
    let refuse = |_: &[ClassSegment]| None;
    assert!(parse_type_with("Lfixture/Outer.Inner;", true, &refuse).is_none());
    assert!(parse_field_gsig_with(
        "Lfixture/Outer.Inner;",
        "Lfixture/Outer$Inner;",
        &Default::default(),
        &refuse
    )
    .is_none());
    assert!(
        parse_member_method_gsig_with("()Lfixture/Result;", &Default::default(), &refuse).is_none()
    );
    assert!(parse_class_gsig_with("Lfixture/Base;", &refuse).is_none());
}
