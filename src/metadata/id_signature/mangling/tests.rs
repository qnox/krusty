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
        placement: Placement::Ordinary,
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
        placement: Placement::Ordinary,
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

/// A companion extension's `ClassId` takes the place of its extension receiver and it is not
/// `#static`; a companion-block member is `#static`. Ids from kotlinc-native 2.4.20.
#[test]
fn a_companion_extension_names_its_class_and_a_companion_block_member_is_static() {
    let package = ["fixture".to_owned(), "signatures".to_owned()];
    let string = Model::Class("kotlin.String", Vec::new());
    let companion = Placement::CompanionExtension {
        class_id: "fixture/signatures/Registry",
    };
    let mut named = function("named", vec![&string], Vec::new());
    named.placement = companion;
    assert_eq!(
        callable_signature(top_level(&package), &named)
            .unwrap()
            .member_id()
            .map(|id| id as i64),
        Some(5_839_546_477_226_615_603)
    );
    let capacity = PropertyShape {
        name: "capacity",
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: companion,
    };
    assert_eq!(
        property_signature(top_level(&package), &capacity)
            .unwrap()
            .member_id()
            .map(|id| id as i64),
        Some(-2_355_867_111_305_705_361)
    );
    let getter = accessor_signature(top_level(&package), &capacity, Accessor::Getter).unwrap();
    assert_eq!(getter.member_id() as i64, 4_019_905_350_587_221_818);

    let mut create = function("create", Vec::new(), Vec::new());
    create.placement = Placement::Static;
    assert_eq!(
        callable_signature(top_level(&package), &create)
            .unwrap()
            .member_id(),
        Some(mangled_id("create#static(){}"))
    );

    let registry = Model::Class("fixture.signatures.Registry", Vec::new());
    named.receiver = Some(&registry);
    assert_eq!(
        callable_signature(top_level(&package), &named)
            .unwrap_err()
            .to_string(),
        "a companion extension has no extension receiver of its own"
    );
}

// The ids below were read from KLIBs kotlinc 2.4.0 wrote for a local or anonymous class that reads
// `x`, a `protected var x` of its abstract base, with the class declared in the callable named.
// The fake override of the base's property has a file-local signature whose id is the hash of the
// mangled enclosing callables followed by the mangled member, never of the class around either.

/// The anonymous class itself, which has no type parameters and contributes nothing to a mangle.
fn anonymous() -> LocalContainer<'static, Model> {
    LocalContainer::Class(Vec::new())
}

fn int() -> Model {
    Model::Class("kotlin.Int", Vec::new())
}

fn property(name: &str) -> PropertyShape<'_, Model> {
    PropertyShape {
        name,
        contexts: Vec::new(),
        receiver: None,
        type_parameters: Vec::new(),
        expect: false,
        placement: Placement::Ordinary,
    }
}

fn getter_id(enclosing: &[LocalContainer<'_, Model>], property: &PropertyShape<'_, Model>) -> u64 {
    local_fake_override_id(
        enclosing,
        &LocalMember::Accessor(property, Accessor::Getter),
    )
    .unwrap()
}

fn property_id(
    enclosing: &[LocalContainer<'_, Model>],
    property: &PropertyShape<'_, Model>,
) -> u64 {
    local_fake_override_id(enclosing, &LocalMember::Property(property)).unwrap()
}

#[test]
fn a_local_fake_override_hashes_the_enclosing_function_and_the_member() {
    // `fun f1(): Base = object : Base() { fun g() = x }`
    let f1 = function("f1", Vec::new(), Vec::new());
    let enclosing = [LocalContainer::Callable(f1), anonymous()];
    let x = property("x");
    assert_eq!(property_id(&enclosing, &x), 10_984_052_984_426_534_210);
    assert_eq!(getter_id(&enclosing, &x), 9_179_919_559_361_952);
    // The member's name is part of the hash; the enclosing function's too.
    let z = property("z");
    assert_eq!(property_id(&enclosing, &z), 18_108_682_745_126_236_731);
    assert_eq!(getter_id(&enclosing, &z), 7_207_252_995_869_796_036);
    let h1 = [
        LocalContainer::Callable(function("h1", Vec::new(), Vec::new())),
        anonymous(),
    ];
    assert_eq!(property_id(&h1, &x), 4_434_011_103_877_386_964);
    assert_eq!(getter_id(&h1, &x), 2_735_225_705_581_274_376);
}

#[test]
fn a_local_fake_override_hashes_the_enclosing_signature() {
    let x = property("x");
    // `fun f1(a: Int)`
    let int = int();
    let with_parameter = [
        LocalContainer::Callable(function("f1", vec![&int], Vec::new())),
        anonymous(),
    ];
    assert_eq!(property_id(&with_parameter, &x), 17_108_885_059_283_319_996);
    assert_eq!(getter_id(&with_parameter, &x), 6_697_465_021_281_376_322);
    // `fun Int.f1()`
    let mut extension = function("f1", Vec::new(), Vec::new());
    extension.receiver = Some(&int);
    let extension = [LocalContainer::Callable(extension), anonymous()];
    assert_eq!(property_id(&extension, &x), 12_329_581_560_742_381_903);
    assert_eq!(getter_id(&extension, &x), 1_366_439_468_820_729_687);
}

#[test]
fn a_local_fake_override_hashes_every_enclosing_function_outermost_first() {
    // `fun f1() = object : Base() { fun h() = object : Base() { fun g() = x } }`
    let nested = [
        LocalContainer::Callable(function("f1", Vec::new(), Vec::new())),
        anonymous(),
        LocalContainer::Callable(function("h", Vec::new(), Vec::new())),
        anonymous(),
    ];
    let x = property("x");
    assert_eq!(property_id(&nested, &x), 16_714_787_241_981_942_725);
    assert_eq!(getter_id(&nested, &x), 11_212_709_492_557_475_373);
}

#[test]
fn a_local_fake_override_of_a_class_in_a_property_initializer_hashes_the_property() {
    // `val top: Base = object : Base() { fun g() = x }`
    let top = property("top");
    let enclosing = [LocalContainer::Property(top), anonymous()];
    let x = property("x");
    assert_eq!(property_id(&enclosing, &x), 8_282_028_456_856_797_756);
    assert_eq!(getter_id(&enclosing, &x), 6_209_393_031_143_520_207);
}

#[test]
fn a_local_fake_override_of_a_class_in_no_function_hashes_the_member_alone() {
    // `class C { init { object : Base() { fun g() = x } } }`
    let x = property("x");
    let enclosing = [anonymous()];
    assert_eq!(property_id(&enclosing, &x), 10_386_213_217_731_204_037);
    assert_eq!(getter_id(&enclosing, &x), 1_482_705_010_654_679_335);
}

#[test]
fn a_local_fake_override_of_a_class_in_an_accessor_hashes_the_accessor() {
    // `var Int.prop: Base get() = object : Base() { fun g() = x }`
    let int = int();
    let mut prop = property("prop");
    prop.receiver = Some(&int);
    let enclosing = [
        LocalContainer::Accessor(prop, Accessor::Getter),
        anonymous(),
    ];
    let x = property("x");
    assert_eq!(property_id(&enclosing, &x), 13_094_006_450_331_863_484);
    assert_eq!(getter_id(&enclosing, &x), 4_728_227_314_972_451_266);
}

#[test]
fn a_type_parameter_of_the_enclosing_function_is_spelled_by_its_depth_from_the_member() {
    // `fun <T> f1(t: T): Base = object : Base() { fun g() = x }`: the member is container 0, the
    // anonymous class 1 and `f1` 2.
    let t = Model::Parameter(0);
    let f1 = function("f1", vec![&t], vec![unbounded(0)]);
    let enclosing = [LocalContainer::Callable(f1), anonymous()];
    let x = property("x");
    assert_eq!(property_id(&enclosing, &x), 265_036_390_071_337_564);
    assert_eq!(getter_id(&enclosing, &x), 5_786_552_929_254_620_021);
}
