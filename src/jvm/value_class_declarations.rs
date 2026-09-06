//! JVM realization facts decoded from a Kotlin value-class declaration.
//!
//! Kotlin metadata is the authority for the underlying property. Both the classpath symbol provider
//! and the JVM representation pass consume this one decoder so frontend discovery and backend
//! realization cannot drift into parallel metadata/descriptor fallbacks.

use crate::jvm::classreader::ClassInfo;
use crate::libraries::{DefaultCallRealization, LibraryMember, MemberRealization};
use crate::types::{Ty, TypeName};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ValueClassDeclaration {
    pub underlying: Ty,
    pub property: Option<String>,
}

pub(super) fn from_class_info(class: &ClassInfo) -> Option<ValueClassDeclaration> {
    let metadata = crate::jvm::metadata::class_inline(class)?;
    let underlying = match metadata.underlying_class.as_deref() {
        Some(classifier) => crate::jvm::classpath::kotlin_name_to_ty(classifier),
        None => class
            .methods
            .iter()
            .find(|method| method.name == "box-impl")
            .and_then(|method| crate::jvm::names::parse_method_descriptor(&method.descriptor))
            .and_then(|(parameters, _)| parameters.first().copied())
            .map(crate::jvm::jvm_libraries::field_desc_to_ty)
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
    };
    let underlying = if metadata.underlying_nullable == Some(false) {
        underlying
    } else {
        Ty::nullable(underlying)
    }
    .canonical_semantic();
    Some(ValueClassDeclaration {
        underlying,
        property: metadata.property_name.clone(),
    })
}

/// Locate the static `$default` realization of a metadata-declared value-class constructor.
///
/// Its Kotlin declaration is still `<init>(source parameters)`, while the JVM emits
/// `constructor-impl(source carriers): carrier` and
/// `constructor-impl$default(source carriers, masks..., marker): carrier`. Ordinary constructor
/// marker overloads are associated by the classpath loader; this distinct static shape belongs to
/// the value-class declaration boundary.
pub(super) fn constructor_default_realization(
    class: &ClassInfo,
    owner: TypeName,
    constructor: &LibraryMember,
) -> Option<DefaultCallRealization> {
    if !matches!(
        constructor.realization,
        MemberRealization::Direct {
            pass_receiver: false
        }
    ) || !constructor
        .call_sig
        .param_defaults
        .iter()
        .any(|default| *default)
    {
        return None;
    }
    let implementation = constructor.physical_name.as_deref()?;
    let (real_params, real_ret) = super::jvm_libraries::parse_method_desc(&constructor.descriptor)?;
    let mask_count = constructor.params.len().div_ceil(32).max(1);
    let bridge_name = format!("{implementation}$default");
    class.methods.iter().find_map(|method| {
        if method.name != bridge_name || !method.is_static() {
            return None;
        }
        let (parameters, ret) = super::jvm_libraries::parse_method_desc(&method.descriptor)?;
        let masks_start = real_params.len();
        let marker = parameters.last().copied()?;
        let matches = ret == real_ret
            && parameters.len() == real_params.len() + mask_count + 1
            && parameters[..masks_start] == real_params
            && parameters[masks_start..masks_start + mask_count]
                .iter()
                .all(|parameter| *parameter == Ty::Int)
            && marker.obj_internal().is_some_and(|classifier| {
                classifier.matches("kotlin/jvm/internal/DefaultConstructorMarker")
            });
        matches.then(|| DefaultCallRealization {
            owner,
            name: bridge_name.clone(),
            descriptor: method.descriptor.clone(),
            declaration_owner: owner,
            real_params: real_params.clone(),
            mask_count,
            ret: real_ret,
            suspend: false,
        })
    })
}
