use super::*;
use crate::compilation_target::CompilationTarget;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use crate::libraries::{CompilerIntrinsic, LibraryCallable};
use crate::types::{type_name, SemanticCallableOwner};

fn wasm_stdlibs() -> Vec<KlibLibraries> {
    [CompilationTarget::WasmJs, CompilationTarget::WasmWasi]
        .into_iter()
        .map(|target| {
            let path = crate::toolchain::kotlin_stdlib_klib(target)
                .expect("the exact target stdlib must be provisioned");
            KlibLibraries::open(&[KlibLibraryInput::platform_stdlib(
                path,
                KlibStdlibSemantics::Wasm,
            )])
            .unwrap()
        })
        .collect()
}

/// The provider identity of `owner`'s only member function `name`.
fn member(source: &dyn SymbolSource, owner: &str, name: &str) -> ExternalCallableId {
    let (functions, _) = declared_member_declarations(source, Ty::obj(owner), name).into_parts();
    let [function] = functions.overloads.as_slice() else {
        panic!("one `{owner}.{name}`, found {}", functions.overloads.len());
    };
    function
        .callable
        .external_identity
        .expect("a KLIB member carries its provider identity")
}

/// The provider identity of the getter of `owner`'s only member property `name`.
fn getter(source: &dyn SymbolSource, owner: &str, name: &str) -> ExternalCallableId {
    let (_, properties) = declared_member_declarations(source, Ty::obj(owner), name).into_parts();
    let [property] = properties.overloads.as_slice() else {
        panic!("one `{owner}.{name}`, found {}", properties.overloads.len());
    };
    property
        .getter
        .external_identity
        .expect("a KLIB property's getter carries its provider identity")
}

fn declaring_owner(callable: &LibraryCallable) -> Option<TypeName> {
    match callable.declaration_owner {
        Some(SemanticCallableOwner::Classifier(owner)) => Some(owner),
        _ => None,
    }
}

#[test]
fn each_number_primitive_runs_its_own_conversion_override() {
    for libraries in wasm_stdlibs() {
        let number = type_name("kotlin/Number");
        let identity = member(&libraries, "kotlin/Number", "toLong");
        let overrides = builtin_overrides(&libraries, number, "toLong", identity);
        let receivers = overrides
            .iter()
            .map(|implementation| implementation.receiver)
            .collect::<Vec<_>>();
        assert_eq!(
            receivers,
            [
                Ty::Byte,
                Ty::Short,
                Ty::Int,
                Ty::Long,
                Ty::Float,
                Ty::Double
            ]
        );
        for implementation in &overrides {
            let declaration = implementation
                .declaration
                .as_ref()
                .expect("every primitive classifier overrides `Number.toLong`");
            assert_eq!(
                declaring_owner(&declaration.callable),
                implementation.receiver.kotlin_class_internal()
            );
            assert_eq!(
                operation(&declaration.callable),
                Some(CompilerIntrinsic::NumericConversion)
            );
        }
    }
}

fn operation(callable: &LibraryCallable) -> Option<CompilerIntrinsic> {
    callable
        .compiler_intrinsic
        .or(callable.member_realization.compiler_intrinsic())
}

#[test]
fn every_primitive_compares_through_its_own_comparable_override() {
    for libraries in wasm_stdlibs() {
        let comparable = type_name("kotlin/Comparable");
        let identity = member(&libraries, "kotlin/Comparable", "compareTo");
        let overrides = builtin_overrides(&libraries, comparable, "compareTo", identity);
        let receivers = overrides
            .iter()
            .map(|implementation| implementation.receiver)
            .collect::<Vec<_>>();
        assert_eq!(receivers, REPRESENTED_BUILTINS);
        for implementation in &overrides {
            let declaration = implementation
                .declaration
                .as_ref()
                .expect("every builtin classifier overrides `Comparable.compareTo`");
            assert_eq!(
                declaring_owner(&declaration.callable),
                implementation.receiver.kotlin_class_internal()
            );
            assert_eq!(
                declaration.identity.params.as_ref(),
                [implementation.receiver]
            );
            // A string and an unsigned integer compare in a body of their own.
            let has_body = matches!(
                implementation.receiver,
                Ty::String | Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong
            );
            assert_eq!(
                operation(&declaration.callable),
                (!has_body).then_some(CompilerIntrinsic::PrimitiveCompare)
            );
        }
    }
}

#[test]
fn a_primitive_classifier_own_member_has_no_overrides_below_it() {
    for libraries in wasm_stdlibs() {
        let identity = member(&libraries, "kotlin/Int", "toLong");
        assert!(
            builtin_overrides(&libraries, type_name("kotlin/Int"), "toLong", identity).is_empty()
        );
    }
}

#[test]
fn a_member_of_a_classifier_no_primitive_is_below_has_no_overrides() {
    for libraries in wasm_stdlibs() {
        let identity = member(&libraries, "kotlin/Throwable", "toString");
        assert!(builtin_overrides(
            &libraries,
            type_name("kotlin/Throwable"),
            "toString",
            identity
        )
        .is_empty());
    }
}

#[test]
fn a_string_reads_its_own_length_and_chars_through_char_sequence() {
    for libraries in wasm_stdlibs() {
        let char_sequence = type_name("kotlin/CharSequence");
        for (name, identity, intrinsic, params, ret) in [
            (
                "length",
                getter(&libraries, "kotlin/CharSequence", "length"),
                CompilerIntrinsic::StringLength,
                &[][..],
                Ty::Int,
            ),
            (
                "get",
                member(&libraries, "kotlin/CharSequence", "get"),
                CompilerIntrinsic::StringGet,
                &[Ty::Int][..],
                Ty::Char,
            ),
        ] {
            let overrides = builtin_overrides(&libraries, char_sequence, name, identity);
            let [implementation] = overrides.as_slice() else {
                panic!("only `String` is a builtin `CharSequence`");
            };
            assert_eq!(implementation.receiver, Ty::String);
            let declaration = implementation
                .declaration
                .as_ref()
                .expect("`String` overrides every member of `CharSequence`");
            assert_eq!(
                declaring_owner(&declaration.callable),
                Some(type_name("kotlin/String"))
            );
            assert_eq!(operation(&declaration.callable), Some(intrinsic));
            assert_eq!(declaration.identity.params.as_ref(), params);
            assert_eq!(declaration.identity.ret, ret);
        }
    }
}

#[test]
fn the_builtin_subtypes_of_a_classifier_are_the_builtin_classes_below_it() {
    for libraries in wasm_stdlibs() {
        assert_eq!(
            builtin_subtypes(&libraries, type_name("kotlin/Comparable")),
            REPRESENTED_BUILTINS
        );
        assert_eq!(
            builtin_subtypes(&libraries, type_name("kotlin/CharSequence")),
            [Ty::String]
        );
        assert_eq!(
            builtin_subtypes(&libraries, type_name("kotlin/Number")),
            [
                Ty::Byte,
                Ty::Short,
                Ty::Int,
                Ty::Long,
                Ty::Float,
                Ty::Double
            ]
        );
        assert!(builtin_subtypes(&libraries, type_name("kotlin/collections/Iterable")).is_empty());
        // A builtin class is not its own proper subtype.
        assert!(builtin_subtypes(&libraries, type_name("kotlin/Int")).is_empty());
    }
}
