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
use crate::jvm::method_descriptors::{jvm_declared_ty, jvm_tys};
use crate::jvm::names::method_descriptor;
use crate::jvm::value_classes::collection_stub_replacements::StubRealization;
use crate::types::{ty_subst_applied_arguments, Ty, TypeName};

use super::{physical_stub_result, Face, ReadOnlyStub, StubBody};

const MUTABLE_METHODS_2_4: &str = include_str!("mutable_methods_2_4.tsv");

/// One row of kotlinc's mutable-only JDK signatures.
struct MutableMethod {
    jvm_owner: TypeName,
    name: &'static str,
    descriptor: &'static str,
    parameters: Box<[Qualifier]>,
    result: Qualifier,
}

/// kotlinc's predefined top-level qualifier of one position.
#[derive(Clone, Copy)]
enum Qualifier {
    /// Not enhanced: the Java type as declared.
    Platform,
    /// `NOT_PLATFORM`: the declared type, its nullability that of the substituted argument.
    Declared,
    /// `NOT_NULL`: the declared type made definitely non-null.
    NotNull,
    /// `NULLABLE`: the declared type made nullable.
    Nullable,
}

impl Qualifier {
    fn parse(spelling: &str) -> Self {
        match spelling {
            "platform" => Self::Platform,
            "declared" => Self::Declared,
            "not-null" => Self::NotNull,
            "nullable" => Self::Nullable,
            _ => panic!("invalid mutable JDK member qualifier `{spelling}`"),
        }
    }

    fn enhance(self, ty: Ty) -> Ty {
        match self {
            Self::Platform | Self::Declared => ty,
            Self::NotNull => ty.non_null(),
            Self::Nullable => Ty::nullable(ty),
        }
    }
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
                let (
                    Some(owner),
                    Some(name),
                    Some(descriptor),
                    Some(parameters),
                    Some(result),
                    None,
                ) = (
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                    fields.next(),
                )
                else {
                    panic!("invalid mutable JDK member policy row {}", index + 1);
                };
                let parameters = if parameters == "-" {
                    Box::default()
                } else {
                    parameters.split(',').map(Qualifier::parse).collect()
                };
                MutableMethod {
                    jvm_owner: crate::types::type_name(owner),
                    name,
                    descriptor,
                    parameters,
                    result: Qualifier::parse(result),
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
    assert_eq!(
        declared_parameters.len(),
        row.parameters.len(),
        "a mutable JDK member policy row qualifies every parameter of its descriptor"
    );
    let parameters: Vec<Ty> = declared_parameters
        .iter()
        .zip(row.parameters.iter())
        .map(|(ty, qualifier)| specialized_slot(*ty, *qualifier, bindings))
        .collect();
    let result = specialized_slot(declared_result, row.result, bindings);
    let realized = StubRealization {
        name: method.name.clone(),
        parameters: jvm_tys(&parameters),
        result: physical_stub_result(result, false),
        result_box: None,
    };
    let (erased_parameters, erased_result) =
        crate::jvm::jvm_libraries::parse_method_desc(&method.descriptor)?;
    let erased_parameters = jvm_tys(&erased_parameters);
    let erased_result = jvm_declared_ty(&erased_result);
    let bridge = (method_descriptor(&erased_parameters, erased_result)
        != method_descriptor(&realized.parameters, realized.result))
    .then_some((erased_parameters, erased_result));
    Some(ReadOnlyStub {
        name: method.name.clone(),
        physical_name: method.name.clone(),
        realized,
        body: StubBody::Throwing,
        override_parameters: parameters.clone(),
        parameter_names: (0..parameters.len())
            .map(crate::jvm::parameter_names::unnamed_java_parameter)
            .collect(),
        parameters,
        result,
        boxed_result: false,
        declaration_site_wildcards: false,
        bridge,
        type_safe: false,
        barrier: None,
    })
}

/// A Java slot specialized to the face's arguments and enhanced by its qualifier. A slot that is the interface's own type
/// parameter holds a reference, so a scalar argument is its box there (`putIfAbsent(String,
/// Integer)` on `Map<String, Int>`), while a value class keeps its own representation.
fn specialized_slot(declared: Ty, qualifier: Qualifier, bindings: &HashMap<String, Ty>) -> Ty {
    // The face's arguments were validated where the class applied it; a Java formal's `Object`
    // bound does not narrow them (`V` of `Map<String, S?>` stays `S?`).
    let substituted = qualifier.enhance(ty_subst_applied_arguments(declared, bindings));
    let is_formal = declared
        .non_null()
        .ty_param_name()
        .is_some_and(|name| bindings.contains_key(name));
    if is_formal && substituted.non_null().is_jvm_scalar() {
        Ty::nullable(substituted.non_null())
    } else {
        substituted
    }
}
