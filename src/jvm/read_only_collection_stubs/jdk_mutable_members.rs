//! The JDK members a mutable collection face adds to its Kotlin builtin declaration.
//!
//! kotlinc's mapped scope gives a mutable builtin (`MutableCollection`, `MutableList`,
//! `MutableMap`) the default members its JDK interface declares for mutation, the exact
//! signatures of `JvmBuiltInsSignatures.MUTABLE_METHOD_SIGNATURES`. They are ordinary members of
//! the mutable declaration, so `CollectionStubMethodLowering` realizes each one a read-only face
//! leaves unimplemented as a throwing stub, after the builtin's own members and in the JDK
//! interface's declaration order. Which of them exist, and their shapes, come from the JDK on the
//! classpath: `addFirst` exists only from Java 21 on, and a JDK without `removeIf` contributes
//! nothing. kotlinc's predefined enhancement of each signature then fixes the nullability of its
//! positions (`merge`'s value is `V & Any`, its result `V?`), which a value-class position's
//! mangled name reflects.

use crate::fx_hash::HashMap;
use crate::jvm::classpath::Classpath;
use crate::jvm::classreader::MethodSig;
use crate::jvm::jvm_class_map::to_jvm_type_name;
use crate::jvm::mapped_builtin_declarations::enhance_jdk_signature;
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::override_results::scalar_over_reference;
use crate::jvm::value_classes::collection_stub_replacements::StubRealization;
use crate::types::{ty_subst_applied_arguments, Ty, TypeName};

use super::{Face, ReadOnlyStub, StubBody};

const MUTABLE_METHODS_2_4: &str = include_str!("mutable_methods_2_4.tsv");

/// One row of kotlinc's mutable-only JDK signatures.
struct MutableMethod {
    jvm_owner: TypeName,
    name: &'static str,
    descriptor: &'static str,
}

fn mutable_methods() -> &'static [MutableMethod] {
    static METHODS: std::sync::OnceLock<Box<[MutableMethod]>> = std::sync::OnceLock::new();
    METHODS.get_or_init(|| {
        MUTABLE_METHODS_2_4
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|(index, line)| {
                let mut fields = line.split('\t');
                let (Some(owner), Some(name), Some(descriptor), None) =
                    (fields.next(), fields.next(), fields.next(), fields.next())
                else {
                    panic!("invalid mutable JDK member policy row {}", index + 1);
                };
                MutableMethod {
                    jvm_owner: crate::types::type_name(owner),
                    name,
                    descriptor,
                }
            })
            .collect()
    })
}

/// The throwing stubs for the mutable-only JDK members of `face`'s JDK interface, substituted with
/// the read-only face's type arguments. A member the builtin declaration already declares
/// (`MutableMap.remove(key, value)`) is one declaration and is left to `builtin`.
pub(super) fn jdk_mutable_stubs(
    classpath: &Classpath,
    face: Face,
    arguments: &[Ty],
    builtin: &[ReadOnlyStub],
) -> Vec<ReadOnlyStub> {
    let owner = to_jvm_type_name(face.mutable);
    let Some(info) = classpath.find_name(owner) else {
        return Vec::new();
    };
    let formals = info
        .signature
        .as_deref()
        .and_then(crate::jvm::jvm_libraries::parse_class_gsig)
        .map(|(formals, _, _)| formals)
        .unwrap_or_default();
    assert_eq!(
        formals.len(),
        arguments.len(),
        "a mapped collection interface and its Kotlin face share their type parameters"
    );
    let bindings: HashMap<String, Ty> =
        formals.into_iter().zip(arguments.iter().copied()).collect();
    info.methods
        .iter()
        .filter(|method| method.is_public() && !method.is_static())
        .filter_map(|method| {
            let row = mutable_methods().iter().find(|row| {
                row.jvm_owner == owner
                    && row.name == method.name
                    && row.descriptor == method.descriptor
            })?;
            Some((method, row))
        })
        .filter(|(method, _)| {
            !builtin.iter().any(|stub| {
                stub.physical_name == method.name && stub.erased_descriptor() == method.descriptor
            })
        })
        .filter_map(|(method, row)| stub(method, row, &bindings))
        .collect()
}

fn stub(
    method: &MethodSig,
    row: &MutableMethod,
    bindings: &HashMap<String, Ty>,
) -> Option<ReadOnlyStub> {
    let (declared_parameters, declared_result) = match method.signature.as_deref() {
        Some(signature) => {
            let declared = crate::jvm::jvm_libraries::parse_method_gsig(signature)?;
            (declared.params, declared.ret)
        }
        None => crate::jvm::jvm_libraries::parse_method_desc(&method.descriptor)?,
    };
    let (enhanced_parameters, enhanced_result) = enhance_jdk_signature(
        row.jvm_owner,
        &method.name,
        &method.descriptor,
        &declared_parameters,
        declared_result,
    );
    let parameters: Vec<Ty> = enhanced_parameters
        .iter()
        .map(|ty| ty_subst_applied_arguments(*ty, bindings))
        .collect();
    let result = ty_subst_applied_arguments(enhanced_result, bindings);
    let boxed_parameters = parameters
        .iter()
        .zip(&declared_parameters)
        .map(|(&parameter, &declared)| scalar_over_reference(parameter, declared))
        .collect();
    let boxed_result = scalar_over_reference(result, declared_result);
    let (erased_parameters, erased_result) =
        crate::jvm::jvm_libraries::parse_method_desc(&method.descriptor)?;
    let erased_parameters = jvm_tys(&erased_parameters);
    let erased_result = jvm_declared_ty(&erased_result);
    let mut stub = ReadOnlyStub {
        name: method.name.clone(),
        physical_name: method.name.clone(),
        realized: StubRealization {
            name: method.name.clone(),
            parameters: Vec::new(),
            result: Ty::Unit,
            result_box: None,
        },
        body: StubBody::Throwing,
        override_parameters: parameters.clone(),
        parameter_names: (0..parameters.len())
            .map(crate::jvm::parameter_names::unnamed_java_parameter)
            .collect(),
        parameters,
        boxed_parameters,
        result,
        boxed_result,
        declaration_site_wildcards: false,
        bridge: None,
        type_safe: false,
        barrier: None,
    };
    stub.realized.parameters = stub.physical_parameters();
    stub.realized.result = stub.physical_result();
    stub.bridge =
        (method.descriptor != stub.descriptor()).then_some((erased_parameters, erased_result));
    Some(stub)
}
