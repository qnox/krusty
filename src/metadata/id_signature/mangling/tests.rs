use super::*;

/// A declaration model of its own, unrelated to decoded metadata. Type parameters are numbered
/// declarations; their spelling is not part of the model at all.
enum Model {
    Class(&'static str, Vec<Model>),
    Parameter(u32),
}

impl SignatureType for Model {
    type ParameterId = u32;

    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError> {
        Ok(match self {
            Model::Class(fq_name, arguments) => TypeView::Class {
                fq_name: Cow::Borrowed(fq_name),
                arguments: arguments.iter().collect(),
                nullable: false,
            },
            Model::Parameter(id) => TypeView::Parameter {
                id: *id,
                nullable: false,
            },
        })
    }
}

fn unbounded(id: u32) -> TypeParameterShape<'static, Model> {
    TypeParameterShape {
        id,
        bounds: Vec::new(),
    }
}

fn function<'a>(
    name: &'a str,
    params: Vec<&'a Model>,
    type_parameters: Vec<TypeParameterShape<'a, Model>>,
) -> CallableShape<'a, Model> {
    CallableShape {
        name,
        contexts: Vec::new(),
        receiver: None,
        params,
        vararg: None,
        type_parameters,
        expect: false,
        static_member: false,
    }
}

fn top_level(package: &[String]) -> DeclarationContainer<'_, Model> {
    DeclarationContainer {
        package,
        classes: &[],
        native_interop_library: false,
    }
}

fn mangled_id(text: &str) -> u64 {
    city_hash::city_hash64(text.as_bytes())
}

#[test]
fn any_declaration_model_mangles_through_its_type_view() {
    // `kotlin.comparisons.maxOf<T : Comparable<T>>(T, T)` in the 2.4.20 stdlib KLIB.
    let bound = Model::Class("kotlin.Comparable", vec![Model::Parameter(0)]);
    let t = Model::Parameter(0);
    let package = ["kotlin".to_owned(), "comparisons".to_owned()];
    let max_of = function(
        "maxOf",
        vec![&t, &t],
        vec![TypeParameterShape {
            id: 0,
            bounds: vec![&bound],
        }],
    );
    let signature = callable_signature(top_level(&package), &max_of).unwrap();
    assert_eq!(
        signature.member_id().map(|id| id as i64),
        Some(-5_030_133_889_946_118_250)
    );
    assert_eq!(signature.mask(), 0);
}

#[test]
fn a_type_parameter_is_located_by_identity_across_scopes() {
    // `class Box<T> { fun <T> shadow(t: T); fun keep(t: T) }`: the class parameter is
    // declaration 0, the member's is declaration 1. Both are spelled `T` in source.
    let package = ["p".to_owned()];
    let classes = [ClassScope {
        name: "Box",
        type_parameters: vec![unbounded(0)],
        expect: false,
    }];
    let inside = DeclarationContainer {
        package: &package,
        classes: &classes,
        native_interop_library: false,
    };
    let member_parameter = Model::Parameter(1);
    let class_parameter = Model::Parameter(0);
    let shadow = function("shadow", vec![&member_parameter], vec![unbounded(1)]);
    let keep = function("keep", vec![&class_parameter], Vec::new());
    let reaches_out = function("reachesOut", vec![&class_parameter], vec![unbounded(1)]);
    assert_eq!(
        callable_signature(inside, &shadow).unwrap().member_id(),
        Some(mangled_id("shadow(0:0){0§<kotlin.Any?>}"))
    );
    assert_eq!(
        callable_signature(inside, &keep).unwrap().member_id(),
        Some(mangled_id("keep(1:0){}"))
    );
    assert_eq!(
        callable_signature(inside, &reaches_out)
            .unwrap()
            .member_id(),
        Some(mangled_id("reachesOut(1:0){0§<kotlin.Any?>}"))
    );
}

#[test]
fn an_unknown_type_parameter_is_an_error() {
    let package = ["p".to_owned()];
    let stray = Model::Parameter(7);
    let broken = function("f", vec![&stray], vec![unbounded(0)]);
    assert_eq!(
        callable_signature(top_level(&package), &broken)
            .unwrap_err()
            .to_string(),
        "type parameter 7 is not in scope"
    );
}

#[test]
fn expect_is_recursive_from_the_classes_around_a_declaration() {
    let package = ["p".to_owned()];
    let int = Model::Class("kotlin.Int", Vec::new());
    let classes = [
        ClassScope {
            name: "Outer",
            type_parameters: Vec::new(),
            expect: true,
        },
        ClassScope {
            name: "Nested",
            type_parameters: Vec::new(),
            expect: false,
        },
    ];
    let nested = DeclarationContainer {
        package: &package,
        classes: &classes,
        native_interop_library: false,
    };
    assert_eq!(class_signature(top_level(&package), &classes[0]).mask(), 1);
    assert_eq!(
        class_signature(
            DeclarationContainer {
                package: &package,
                classes: &classes[..1],
                native_interop_library: false,
            },
            &classes[1]
        )
        .mask(),
        1
    );
    let member = callable_signature(nested, &function("f", vec![&int], Vec::new())).unwrap();
    assert_eq!(member.mask(), 1);
    assert_eq!(enum_entry_signature(nested, "ENTRY").mask(), 1);

    let mut own = function("g", Vec::new(), Vec::new());
    own.expect = true;
    let top = callable_signature(top_level(&package), &own).unwrap();
    assert_eq!(top.mask(), 1);
    assert_eq!(top.member_id(), Some(mangled_id("g(){}")));
}

#[test]
fn every_declaration_of_an_interop_library_is_marked() {
    let package = ["platform".to_owned(), "posix".to_owned()];
    let interop = DeclarationContainer {
        package: &package,
        classes: &[],
        native_interop_library: true,
    };
    let int = Model::Class("kotlin.Int", Vec::new());
    let getpid = callable_signature(interop, &function("getpid", Vec::new(), Vec::new())).unwrap();
    assert_eq!(getpid.mask(), 4);
    let errno = PropertyShape {
        name: "errno",
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: true,
        static_member: false,
    };
    let setter = accessor_signature(interop, &errno, Accessor::Setter { value: &int }).unwrap();
    assert_eq!(setter.mask(), 5);
    assert_eq!(setter.property().mask(), 5);
    assert_eq!(setter.member_id(), mangled_id("<set-errno>(kotlin.Int){}"));
}

#[test]
fn a_vararg_index_outside_the_value_parameters_is_an_error() {
    let package = ["p".to_owned()];
    let array = Model::Class(
        "kotlin.Array",
        vec![Model::Class("kotlin.String", Vec::new())],
    );
    let mut varargs = function("f", vec![&array], Vec::new());
    varargs.vararg = Some(0);
    assert_eq!(
        callable_signature(top_level(&package), &varargs)
            .unwrap()
            .member_id(),
        Some(mangled_id("f(kotlin.Array<kotlin.String>...){}"))
    );
    varargs.vararg = Some(1);
    assert_eq!(
        callable_signature(top_level(&package), &varargs)
            .unwrap_err()
            .to_string(),
        "f marks parameter 1 as vararg but has 1 value parameters"
    );
}
