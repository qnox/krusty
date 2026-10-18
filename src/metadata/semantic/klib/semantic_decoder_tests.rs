//! Exact wire fixtures for the KLIB semantic declaration decoder.

use super::super::parse_package_fragment_checked;

fn varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        out.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            return;
        }
    }
}

fn int_field(out: &mut Vec<u8>, number: u64, value: u64) {
    varint(out, number << 3);
    varint(out, value);
}

fn bytes_field(out: &mut Vec<u8>, number: u64, value: &[u8]) {
    varint(out, number << 3 | 2);
    varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn class_type(qname: u64) -> Vec<u8> {
    let mut ty = Vec::new();
    int_field(&mut ty, 6, qname);
    ty
}

fn type_argument_id(id: u64) -> Vec<u8> {
    let mut argument = Vec::new();
    int_field(&mut argument, 3, id);
    argument
}

fn type_table(types: &[Vec<u8>], first_nullable: Option<u64>) -> Vec<u8> {
    let mut table = Vec::new();
    for ty in types {
        bytes_field(&mut table, 1, ty);
    }
    if let Some(first_nullable) = first_nullable {
        int_field(&mut table, 2, first_nullable);
    }
    table
}

fn qname(short: u64, parent: Option<u64>, kind: u64) -> Vec<u8> {
    let mut name = Vec::new();
    if let Some(parent) = parent {
        int_field(&mut name, 1, parent);
    }
    int_field(&mut name, 2, short);
    int_field(&mut name, 3, kind);
    name
}

fn fragment(strings: &[&str], qnames: &[Vec<u8>], package: &[u8]) -> Vec<u8> {
    let mut string_table = Vec::new();
    for string in strings {
        bytes_field(&mut string_table, 1, string.as_bytes());
    }
    let mut qname_table = Vec::new();
    for qname in qnames {
        bytes_field(&mut qname_table, 1, qname);
    }
    let mut fragment = Vec::new();
    bytes_field(&mut fragment, 1, &string_table);
    bytes_field(&mut fragment, 2, &qname_table);
    bytes_field(&mut fragment, 3, package);
    fragment
}

fn fragment_with_classes(strings: &[&str], qnames: &[Vec<u8>], classes: &[Vec<u8>]) -> Vec<u8> {
    let mut fragment = fragment(strings, qnames, &[]);
    for class in classes {
        bytes_field(&mut fragment, 4, class);
    }
    fragment
}

fn kotlin_types(names: &[&'static str]) -> (Vec<&'static str>, Vec<Vec<u8>>) {
    let mut strings = vec!["f", "p", "kotlin"];
    let mut qnames = vec![qname(2, None, 1)];
    for name in names {
        let short = strings.len() as u64;
        strings.push(*name);
        qnames.push(qname(short, Some(0), 0));
    }
    (strings, qnames)
}

fn function_with_return(return_body: Option<&[u8]>, return_id: Option<u64>) -> Vec<u8> {
    let mut function = Vec::new();
    int_field(&mut function, 2, 0);
    if let Some(return_body) = return_body {
        bytes_field(&mut function, 3, return_body);
    }
    if let Some(return_id) = return_id {
        int_field(&mut function, 7, return_id);
    }
    function
}

fn property_with_return(name: u64, return_body: &[u8]) -> Vec<u8> {
    let mut property = Vec::new();
    int_field(&mut property, 2, name);
    bytes_field(&mut property, 3, return_body);
    property
}

fn package_with_function(function: &[u8], table: Option<&[u8]>) -> Vec<u8> {
    let mut package = Vec::new();
    bytes_field(&mut package, 3, function);
    if let Some(table) = table {
        bytes_field(&mut package, 30, table);
    }
    package
}

fn package_with_type_alias(alias: &[u8]) -> Vec<u8> {
    let mut package = Vec::new();
    bytes_field(&mut package, 5, alias);
    package
}

fn value_parameter(name: u64, ty: &[u8], flags: u64) -> Vec<u8> {
    let mut parameter = Vec::new();
    if flags != 0 {
        int_field(&mut parameter, 1, flags);
    }
    int_field(&mut parameter, 2, name);
    bytes_field(&mut parameter, 3, ty);
    parameter
}

fn exact_error(bytes: &[u8]) -> (usize, String) {
    let error = parse_package_fragment_checked(bytes)
        .err()
        .expect("fixed invalid fragment must fail");
    (error.offset, error.detail)
}

#[test]
fn first_nullable_is_part_of_the_selected_semantic_type_table() {
    let (strings, qnames) = kotlin_types(&["String"]);
    let table = type_table(&[class_type(1)], Some(0));
    let function = function_with_return(None, Some(0));
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function, Some(&table)),
    );

    let package = parse_package_fragment_checked(&bytes).expect("valid nullable table fixture");
    assert_eq!(package.functions.len(), 1);
    assert_eq!(package.functions[0].ret.internal(), Some("kotlin/String"));
    assert!(package.functions[0].ret.nullable());
}

#[test]
fn context_and_value_parameter_inline_modifiers_are_retained_exactly() {
    use crate::types::InlineParameterModifier::{Crossinline, Noinline, None as Plain};

    let (strings, qnames) = kotlin_types(&["Unit"]);
    let unit = class_type(1);
    let mut function = function_with_return(Some(&unit), None);
    // A defaulted crossinline context parameter, followed by plain and noinline values.
    bytes_field(
        &mut function,
        13,
        &value_parameter(1, &unit, (1 << 1) | (1 << 2)),
    );
    bytes_field(&mut function, 6, &value_parameter(1, &unit, 0));
    bytes_field(&mut function, 6, &value_parameter(1, &unit, 1 << 3));
    let bytes = fragment(&strings, &qnames, &package_with_function(&function, None));

    let package = parse_package_fragment_checked(&bytes).expect("valid parameter modifiers");
    let [function] = package.functions.as_slice() else {
        panic!("one function is decoded")
    };
    assert_eq!(function.param_defaults, [true, false, false]);
    assert_eq!(function.inline_modifiers, [Crossinline, Plain, Noinline]);
    assert_eq!(function.context_count, 1);
}

#[test]
fn type_alias_expansion_retains_definitely_non_null_argument_path() {
    let strings = ["Alias", "T", "kotlin", "Array"];
    let qnames = [qname(2, None, 1), qname(3, Some(0), 0)];
    let mut parameter = Vec::new();
    int_field(&mut parameter, 1, 0);
    int_field(&mut parameter, 2, 1);
    let mut dnn_parameter = Vec::new();
    int_field(&mut dnn_parameter, 1, 2);
    int_field(&mut dnn_parameter, 7, 0);
    let mut argument = Vec::new();
    bytes_field(&mut argument, 2, &dnn_parameter);
    let mut expansion = class_type(1);
    bytes_field(&mut expansion, 2, &argument);
    let mut alias = Vec::new();
    int_field(&mut alias, 2, 0);
    bytes_field(&mut alias, 3, &parameter);
    bytes_field(&mut alias, 4, &expansion);
    bytes_field(&mut alias, 6, &expansion);
    let bytes = fragment(&strings, &qnames, &package_with_type_alias(&alias));

    let package = parse_package_fragment_checked(&bytes).expect("valid DNN alias expansion");
    assert_eq!(package.type_aliases.len(), 1);
    let spelling = &package.type_aliases[0].expansion_spelling;
    assert!(!spelling.definitely_non_null);
    assert_eq!(spelling.args.len(), 1);
    assert!(spelling.args[0].definitely_non_null);
}

#[test]
fn an_inner_type_appends_its_outer_type_arguments() {
    let strings = ["f", "p", "Outer", "Inner", "kotlin", "String", "Int"];
    let qnames = [
        qname(1, None, 1),
        qname(2, Some(0), 0),
        qname(3, Some(1), 0),
        qname(4, None, 1),
        qname(5, Some(3), 0),
        qname(6, Some(3), 0),
    ];
    let mut outer = class_type(1);
    bytes_field(&mut outer, 2, &type_argument_id(0));
    let mut inner = class_type(2);
    bytes_field(&mut inner, 2, &type_argument_id(1));
    int_field(&mut inner, 11, 2);
    let table = type_table(&[class_type(4), class_type(5), outer, inner], None);
    let function = function_with_return(None, Some(3));
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function, Some(&table)),
    );

    let package = parse_package_fragment_checked(&bytes).expect("valid inner type");
    assert_eq!(
        package.functions[0].ret,
        crate::metadata::semantic::KotlinType::Class {
            internal: "p/Outer.Inner".to_string(),
            args: vec![
                crate::metadata::semantic::KotlinType::class("kotlin/Int"),
                crate::metadata::semantic::KotlinType::class("kotlin/String"),
            ],
            nullable: false,
            shape: Default::default(),
        }
    );
}

#[test]
fn companion_extension_receiver_forms_are_the_semantic_receiver_contract() {
    let (strings, qnames) = kotlin_types(&["Unit", "Float"]);
    let unit = class_type(1);
    let float = class_type(2);
    let table = type_table(std::slice::from_ref(&float), None);

    for (function_field, property_field, table_form) in [(14, 18, false), (15, 19, true)] {
        let mut function = function_with_return(Some(&unit), None);
        let mut property = property_with_return(0, &unit);
        if table_form {
            int_field(&mut function, function_field, 0);
            int_field(&mut property, property_field, 0);
        } else {
            bytes_field(&mut function, function_field, &float);
            bytes_field(&mut property, property_field, &float);
        }
        let mut package = package_with_function(&function, Some(&table));
        bytes_field(&mut package, 4, &property);
        let package = parse_package_fragment_checked(&fragment(&strings, &qnames, &package))
            .expect("valid companion-extension receiver fixture");

        assert_eq!(
            package.functions[0]
                .receiver
                .as_ref()
                .and_then(crate::metadata::semantic::KotlinType::internal),
            Some("kotlin/Float"),
            "function receiver field {function_field}"
        );
        assert!(
            package.functions[0].is_static,
            "function companion receiver field {function_field} establishes associated placement"
        );
        assert_eq!(
            package.properties[0]
                .receiver
                .as_ref()
                .and_then(crate::metadata::semantic::KotlinType::internal),
            Some("kotlin/Float"),
            "property receiver field {property_field}"
        );
        assert!(
            package.properties[0].is_static,
            "property companion receiver field {property_field} establishes associated placement"
        );
    }
}

#[test]
fn companion_extension_receiver_conflicts_are_exact_errors() {
    let (strings, qnames) = kotlin_types(&["Unit", "Float"]);
    let unit = class_type(1);
    let float = class_type(2);
    let table = type_table(std::slice::from_ref(&float), None);

    let function_error = |function: Vec<u8>| {
        exact_error(&fragment(
            &strings,
            &qnames,
            &package_with_function(&function, Some(&table)),
        ))
    };
    let mut function = function_with_return(Some(&unit), None);
    bytes_field(&mut function, 5, &float);
    bytes_field(&mut function, 14, &float);
    assert_eq!(
        function_error(function),
        (
            0,
            "function has both ordinary and companion extension receivers".to_string()
        )
    );
    let mut function = function_with_return(Some(&unit), None);
    bytes_field(&mut function, 14, &float);
    bytes_field(&mut function, 14, &float);
    assert_eq!(
        function_error(function),
        (
            0,
            "function companion extension receiver has duplicate inline forms".to_string()
        )
    );
    let mut function = function_with_return(Some(&unit), None);
    bytes_field(&mut function, 14, &float);
    int_field(&mut function, 15, 0);
    assert_eq!(
        function_error(function),
        (
            0,
            "function companion extension receiver has conflicting inline and table type forms"
                .to_string()
        )
    );

    let property_error = |property: Vec<u8>| {
        let mut package = Vec::new();
        bytes_field(&mut package, 4, &property);
        bytes_field(&mut package, 30, &table);
        exact_error(&fragment(&strings, &qnames, &package))
    };
    let mut property = property_with_return(0, &unit);
    bytes_field(&mut property, 5, &float);
    bytes_field(&mut property, 18, &float);
    assert_eq!(
        property_error(property),
        (
            0,
            "property has both ordinary and companion extension receivers".to_string()
        )
    );
    let mut property = property_with_return(0, &unit);
    int_field(&mut property, 19, 0);
    int_field(&mut property, 19, 0);
    assert_eq!(
        property_error(property),
        (
            0,
            "property companion extension receiver has duplicate table forms".to_string()
        )
    );
    let mut property = property_with_return(0, &unit);
    bytes_field(&mut property, 18, &float);
    int_field(&mut property, 19, 0);
    assert_eq!(
        property_error(property),
        (
            0,
            "property companion extension receiver has conflicting inline and table type forms"
                .to_string()
        )
    );
}

/// `.kotlin_builtins` writes a function's annotations to `BuiltInsProtoBuf` field 150, a KLIB
/// to field 170; both decode to the annotation's qualified identity.
#[test]
fn function_annotations_keep_their_qualified_identity() {
    let strings = [
        "f",
        "p",
        "kotlin",
        "Unit",
        "internal",
        "IntrinsicConstEvaluation",
    ];
    let qnames = [
        qname(2, None, 1),
        qname(3, Some(0), 0),
        qname(4, Some(0), 1),
        qname(5, Some(2), 0),
    ];
    let mut annotation = Vec::new();
    int_field(&mut annotation, 1, 3);
    for annotation_field in [150, 170] {
        let mut function = function_with_return(Some(&class_type(1)), None);
        bytes_field(&mut function, annotation_field, &annotation);
        let bytes = fragment(&strings, &qnames, &package_with_function(&function, None));

        let package = parse_package_fragment_checked(&bytes).expect("valid annotated function");
        assert_eq!(
            package.functions[0].annotations,
            vec![crate::types::wk::intrinsic_const_evaluation()],
            "annotation field {annotation_field}"
        );
    }
}

#[test]
fn function_annotations_keep_string_arguments_on_both_declaration_views() {
    let strings = [
        "f",
        "p",
        "kotlin",
        "Unit",
        "wasm",
        "internal",
        "WasmOp",
        "name",
        "I32_REINTERPRET_F32",
        "Holder",
    ];
    let qnames = [
        qname(2, None, 1),
        qname(3, Some(0), 0),
        qname(4, Some(0), 1),
        qname(5, Some(2), 1),
        qname(6, Some(3), 0),
        qname(9, Some(0), 0),
    ];
    let mut value = Vec::new();
    int_field(&mut value, 1, 8);
    int_field(&mut value, 5, 8);
    let mut argument = Vec::new();
    int_field(&mut argument, 1, 7);
    bytes_field(&mut argument, 2, &value);
    let mut annotation = Vec::new();
    int_field(&mut annotation, 1, 4);
    bytes_field(&mut annotation, 2, &argument);
    let mut function = function_with_return(Some(&class_type(1)), None);
    bytes_field(&mut function, 170, &annotation);

    let mut class = Vec::new();
    int_field(&mut class, 3, 5);
    bytes_field(&mut class, 9, &function);
    let package =
        parse_package_fragment_checked(&fragment_with_classes(&strings, &qnames, &[class]))
            .expect("valid string-valued declaration annotation");
    let class = &package.classes["kotlin/Holder"];
    let expected = crate::metadata::semantic::AnnotationApplication {
        identity: crate::types::type_name("kotlin/wasm/internal/WasmOp"),
        arguments: vec![(
            "name".to_owned(),
            crate::metadata::semantic::AnnotationArgument::String("I32_REINTERPRET_F32".to_owned()),
        )],
    };
    assert_eq!(
        class.functions[0].annotation_applications,
        [expected.clone()]
    );
    assert_eq!(class.members[0].annotation_applications, [expected]);
    assert_eq!(
        class.functions[0].annotations,
        [crate::types::type_name("kotlin/wasm/internal/WasmOp")]
    );
}

/// `.kotlin_builtins` field 150 belongs to the property declaration, not its getter. Decode it
/// on that exact member and retain the annotation's qualified identity.
#[test]
fn property_annotation_keeps_its_qualified_identity() {
    let strings = [
        "name",
        "kotlin",
        "String",
        "Enum",
        "internal",
        "IntrinsicConstEvaluation",
    ];
    let qnames = [
        qname(1, None, 1),
        qname(2, Some(0), 0),
        qname(3, Some(0), 0),
        qname(4, Some(0), 1),
        qname(5, Some(3), 0),
    ];
    let mut annotation = Vec::new();
    int_field(&mut annotation, 1, 4);
    let mut property = property_with_return(0, &class_type(1));
    bytes_field(&mut property, 150, &annotation);
    let mut enum_class = Vec::new();
    int_field(&mut enum_class, 3, 2);
    bytes_field(&mut enum_class, 10, &property);

    let package =
        parse_package_fragment_checked(&fragment_with_classes(&strings, &qnames, &[enum_class]))
            .expect("valid annotated property");
    let member = &package.classes["kotlin/Enum"].members[0];
    assert!(member.is_property);
    assert_eq!(member.name, "name");
    assert_eq!(
        member.annotations,
        vec![crate::types::wk::intrinsic_const_evaluation()]
    );
}

#[test]
fn inner_class_supertype_resolves_the_enclosing_class_type_parameter() {
    let strings = ["p", "Outer", "Inner", "T", "Base", "StaticNested"];
    let qnames = [
        qname(0, None, 1),
        qname(1, Some(0), 0),
        qname(2, Some(1), 0),
        qname(4, Some(0), 0),
        qname(5, Some(1), 0),
    ];

    let mut type_parameter = Vec::new();
    int_field(&mut type_parameter, 1, 0);
    int_field(&mut type_parameter, 2, 3);
    let mut outer = Vec::new();
    int_field(&mut outer, 3, 1);
    bytes_field(&mut outer, 5, &type_parameter);

    let mut parameter_type = Vec::new();
    int_field(&mut parameter_type, 7, 0);
    let mut argument = Vec::new();
    int_field(&mut argument, 3, 0);
    let mut base_of_parameter = Vec::new();
    bytes_field(&mut base_of_parameter, 2, &argument);
    int_field(&mut base_of_parameter, 6, 3);
    let table = type_table(&[parameter_type, base_of_parameter], None);
    let mut inner = Vec::new();
    int_field(&mut inner, 1, 530);
    int_field(&mut inner, 2, 1);
    int_field(&mut inner, 3, 2);
    bytes_field(&mut inner, 30, &table);

    // KLIB fragments do not promise enclosing-before-nested declaration order. The inner class
    // still resolves Type.type_parameter = 0 through its enclosing class's declaration scope.
    let bytes = fragment_with_classes(&strings, &qnames, &[inner, outer.clone()]);
    let package = parse_package_fragment_checked(&bytes).expect("valid inner-class type scope");
    assert_eq!(package.classes["p/Outer"].type_params[0].name, "T");
    assert!(package.classes["p/Outer.Inner"].type_params.is_empty());
    assert_eq!(
        package.classes["p/Outer.Inner"].supertype_tys[0].render(),
        "p/Base<T>"
    );

    let mut static_nested = Vec::new();
    int_field(&mut static_nested, 1, 2);
    int_field(&mut static_nested, 2, 1);
    int_field(&mut static_nested, 3, 4);
    bytes_field(&mut static_nested, 30, &table);
    let bytes = fragment_with_classes(&strings, &qnames, &[outer, static_nested]);
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "class supertype references absent type parameter 0".to_string()
        )
    );
}

#[test]
fn dangling_alias_equality_context_and_annotation_references_are_exact_errors() {
    let (strings, qnames) = kotlin_types(&["Unit"]);
    let unit = class_type(1);

    let mut alias = Vec::new();
    int_field(&mut alias, 12, 99);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function_with_return(Some(&alias), None), None),
    );
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "function return type references absent qualified name 99".to_string()
        )
    );

    let mut equality_parameter = Vec::new();
    int_field(&mut equality_parameter, 2, 1);
    bytes_field(&mut equality_parameter, 3, &unit);
    int_field(&mut equality_parameter, 10, 9);
    let mut equality_function = function_with_return(Some(&unit), None);
    bytes_field(&mut equality_function, 6, &equality_parameter);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&equality_function, None),
    );
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "value-parameter equality bound references absent type 9".to_string()
        )
    );

    let mut context_parameter = Vec::new();
    int_field(&mut context_parameter, 2, 1);
    int_field(&mut context_parameter, 5, 9);
    let mut context_function = function_with_return(Some(&unit), None);
    bytes_field(&mut context_function, 13, &context_parameter);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&context_function, None),
    );
    assert_eq!(
        exact_error(&bytes),
        (0, "context parameter references absent type 9".to_string())
    );

    let mut annotation = Vec::new();
    int_field(&mut annotation, 1, 99);
    let mut annotated_function = function_with_return(Some(&unit), None);
    bytes_field(&mut annotated_function, 170, &annotation);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&annotated_function, None),
    );
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "annotation references absent qualified name 99".to_string()
        )
    );
}

#[test]
fn contract_table_is_isolated_from_the_enclosing_signature_table() {
    let (strings, qnames) = kotlin_types(&["Unit", "String"]);
    let package_table = type_table(&[class_type(1)], None);
    let contract_table = type_table(&[class_type(2)], None);
    let mut expression = Vec::new();
    int_field(&mut expression, 2, 1);
    int_field(&mut expression, 5, 0);
    let mut effect = Vec::new();
    bytes_field(&mut effect, 3, &expression);
    let mut contract = Vec::new();
    bytes_field(&mut contract, 1, &effect);
    let mut function = function_with_return(None, Some(0));
    bytes_field(&mut function, 30, &contract_table);
    bytes_field(&mut function, 32, &contract);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function, Some(&package_table)),
    );

    let package = parse_package_fragment_checked(&bytes).expect("valid isolated tables");
    assert_eq!(package.functions[0].ret.internal(), Some("kotlin/Unit"));
    assert_eq!(
        package.functions[0].contract.as_deref(),
        Some(&crate::contracts::Contract {
            effects: vec![crate::contracts::Effect::ConditionalReturns {
                returns: crate::contracts::ReturnsValue::Any,
                conclusion: crate::contracts::Condition::IsType {
                    param: crate::contracts::ParamRef::Param(0),
                    ty: crate::contracts::ConditionType::Metadata(crate::types::Ty::obj(
                        "kotlin/String",
                    )),
                    negated: false,
                },
            }],
        })
    );
}

#[test]
fn contract_type_id_uses_the_containing_table_when_the_function_has_none() {
    let (strings, qnames) = kotlin_types(&["Unit", "String"]);
    let package_table = type_table(&[class_type(1), class_type(2)], None);
    let mut expression = Vec::new();
    int_field(&mut expression, 2, 1);
    int_field(&mut expression, 5, 1);
    let mut effect = Vec::new();
    bytes_field(&mut effect, 3, &expression);
    let mut contract = Vec::new();
    bytes_field(&mut contract, 1, &effect);
    let mut function = function_with_return(None, Some(0));
    bytes_field(&mut function, 32, &contract);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function, Some(&package_table)),
    );

    let package = parse_package_fragment_checked(&bytes)
        .expect("contract type id must resolve against the package table");
    assert_eq!(package.functions.len(), 1);
    assert_eq!(package.functions[0].ret.internal(), Some("kotlin/Unit"));
    assert_eq!(
        package.functions[0].contract.as_deref(),
        Some(&crate::contracts::Contract {
            effects: vec![crate::contracts::Effect::ConditionalReturns {
                returns: crate::contracts::ReturnsValue::Any,
                conclusion: crate::contracts::Condition::IsType {
                    param: crate::contracts::ParamRef::Param(0),
                    ty: crate::contracts::ConditionType::Metadata(crate::types::Ty::obj(
                        "kotlin/String",
                    )),
                    negated: false,
                },
            }],
        })
    );
}

#[test]
fn conflicting_type_forms_invalid_qname_kind_and_variance_are_exact_errors() {
    let (strings, qnames) = kotlin_types(&["Unit"]);
    let unit = class_type(1);
    let table = type_table(std::slice::from_ref(&unit), None);
    let bytes = fragment(
        &strings,
        &qnames,
        &package_with_function(&function_with_return(Some(&unit), Some(0)), Some(&table)),
    );
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "function return type has conflicting inline and table type forms".to_string()
        )
    );

    let invalid_qnames = vec![qname(1, None, 3)];
    let bytes = fragment(
        &["f", "Broken"],
        &invalid_qnames,
        &package_with_function(&function_with_return(Some(&class_type(0)), None), None),
    );
    assert_eq!(
        exact_error(&bytes),
        (0, "qualified-name entry 0 has invalid kind 3".to_string())
    );

    let mut parameter = Vec::new();
    int_field(&mut parameter, 1, 0);
    int_field(&mut parameter, 2, 1);
    int_field(&mut parameter, 4, 3);
    let mut function = function_with_return(Some(&unit), None);
    bytes_field(&mut function, 4, &parameter);
    let bytes = fragment(&strings, &qnames, &package_with_function(&function, None));
    assert_eq!(
        exact_error(&bytes),
        (
            0,
            "invalid type-parameter variance 3 in function declaration".to_string()
        )
    );
}
