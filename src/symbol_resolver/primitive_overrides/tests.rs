use super::*;
use crate::compilation_target::CompilationTarget;
use crate::klib_libraries::{KlibLibraries, KlibLibraryInput, KlibStdlibSemantics};
use crate::libraries::CompilerIntrinsic;
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

/// The provider identity of `owner`'s only member `name`.
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

fn declaring_owner(function: &FunctionInfo) -> Option<TypeName> {
    match function.callable.declaration_owner {
        Some(SemanticCallableOwner::Classifier(owner)) => Some(owner),
        _ => None,
    }
}

#[test]
fn each_number_primitive_runs_its_own_conversion_override() {
    for libraries in wasm_stdlibs() {
        let number = type_name("kotlin/Number");
        let identity = member(&libraries, "kotlin/Number", "toLong");
        let overrides = primitive_overrides(&libraries, number, "toLong", identity);
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
                declaring_owner(declaration),
                implementation.receiver.kotlin_class_internal()
            );
            assert_eq!(
                operation(declaration),
                Some(CompilerIntrinsic::NumericConversion)
            );
        }
    }
}

fn operation(function: &FunctionInfo) -> Option<CompilerIntrinsic> {
    function
        .callable
        .compiler_intrinsic
        .or(function.callable.member_realization.compiler_intrinsic())
}

#[test]
fn every_primitive_compares_through_its_own_comparable_override() {
    for libraries in wasm_stdlibs() {
        let comparable = type_name("kotlin/Comparable");
        let identity = member(&libraries, "kotlin/Comparable", "compareTo");
        let overrides = primitive_overrides(&libraries, comparable, "compareTo", identity);
        let receivers = overrides
            .iter()
            .map(|implementation| implementation.receiver)
            .collect::<Vec<_>>();
        assert_eq!(receivers, PRIMITIVE_TYPES);
        for implementation in &overrides {
            let declaration = implementation
                .declaration
                .as_ref()
                .expect("every primitive classifier overrides `Comparable.compareTo`");
            assert_eq!(
                declaration.semantic_params().as_ref(),
                [implementation.receiver]
            );
            assert_eq!(
                operation(declaration),
                Some(CompilerIntrinsic::PrimitiveCompare)
            );
        }
    }
}

#[test]
fn a_primitive_classifier_own_member_has_no_overrides_below_it() {
    for libraries in wasm_stdlibs() {
        let identity = member(&libraries, "kotlin/Int", "toLong");
        assert!(
            primitive_overrides(&libraries, type_name("kotlin/Int"), "toLong", identity).is_empty()
        );
    }
}

#[test]
fn a_member_of_a_classifier_no_primitive_is_below_has_no_overrides() {
    for libraries in wasm_stdlibs() {
        let identity = member(&libraries, "kotlin/Throwable", "toString");
        assert!(primitive_overrides(
            &libraries,
            type_name("kotlin/Throwable"),
            "toString",
            identity
        )
        .is_empty());
    }
}
