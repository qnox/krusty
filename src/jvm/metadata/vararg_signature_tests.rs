//! Declaration-owned packed-array types in JVM metadata varargs.

use super::parse_function;
use crate::types::Ty;

#[test]
fn function_vararg_signatures_keep_the_declared_packed_array() {
    use crate::metadata::protobuf::Pb as Writer;
    let names = [
        "f",
        "T",
        "kotlin/Array",
        "kotlin/String",
        "kotlin/Unit",
        "kotlin/IntArray",
        "kotlin/Int",
        "items",
    ]
    .map(str::to_owned);
    let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
    for (declared, element, expected) in [
        (
            &[0x30, 0x02, 0x12, 0x06, 0x08, 0x01, 0x12, 0x02, 0x38, 0x00][..],
            &[0x38, 0x00][..],
            Ty::obj_args("kotlin/Array", &[Ty::out_projection(formal)]),
        ),
        (&[0x30, 0x05][..], &[0x30, 0x06][..], Ty::array(Ty::Int)),
    ] {
        for indexed in [false, true] {
            let mut table = Writer::new();
            table.field_bytes(1, declared);
            table.field_bytes(1, element);
            let mut parameter = Writer::new();
            parameter.field_varint(2, 7);
            if indexed {
                parameter.field_varint(5, 0);
                parameter.field_varint(6, 1);
            } else {
                parameter.field_bytes(3, declared);
                parameter.field_bytes(4, element);
            }
            let mut function = Writer::new();
            function.field_varint(2, 0);
            function.field_bytes(3, &[0x30, 0x04]);
            function.field_bytes(4, &[0x08, 0x00, 0x10, 0x01]);
            function.field_message(6, &parameter);
            let parsed = parse_function(function.as_bytes()).expect("vararg declaration");
            let signature = super::build_generic_sig(
                &parsed,
                &[],
                &names,
                &[],
                &[],
                None,
                indexed.then_some(table.as_bytes()),
            )
            .expect("complete metadata signature");
            assert_eq!(signature.params, [expected], "indexed={indexed}");
        }
    }
    let missing_type = parse_function(&[
        0x10, 0x00, 0x1a, 0x02, 0x30, 0x04, 0x32, 0x06, 0x10, 0x07, 0x22, 0x02, 0x30, 0x03,
    ])
    .expect("vararg element without declared type");
    assert!(
        super::build_generic_sig(&missing_type, &[], &names, &[], &[], None, None).is_none(),
        "the vararg element must not manufacture a missing declared array"
    );
}

#[test]
fn selected_stdlib_vararg_signatures_preserve_metadata_projections() {
    let classpath = crate::toolchain::stdlib_classpath();
    let functions = classpath.meta_functions_name(crate::types::type_name(
        "kotlin/collections/CollectionsKt__CollectionsKt",
    ));
    for name in ["listOf", "mutableListOf", "arrayListOf"] {
        let matching: Vec<_> = functions
            .iter()
            .filter(|f| f.kotlin_name == name && f.vararg_index() == Some(0))
            .collect();
        let [function] = matching.as_slice() else {
            panic!("one selected stdlib {name} vararg")
        };
        let signature = function.generic_sig.as_ref().expect("metadata signature");
        let [formal] = signature.formals.as_slice() else {
            panic!("one factory formal")
        };
        let element = Ty::ty_param(formal, signature.primary_formal_bound(0));
        assert_eq!(
            signature.params,
            [Ty::obj_args("kotlin/Array", &[Ty::out_projection(element)])],
            "{name}"
        );
    }
}
