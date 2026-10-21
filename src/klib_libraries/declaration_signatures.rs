//! Exact public `IdSignature`s of the declarations a KLIB provider publishes.

use crate::fx_hash::HashMap;
use crate::libraries::{
    associated_function_parameter_identities, associated_property_parameter_identities,
    function_parameter_identities, property_parameter_identities, FunctionParameterIdentities,
    PropertyParameterIdentities, TypeParameterIdentities,
};
use crate::metadata::id_signature::{
    package_function_signature, package_property_accessor_signature, package_property_signature,
    KlibAccessorIdSignature, KlibPublicIdSignature, MetadataAccessor, MetadataContainer,
};
use crate::metadata::semantic::{KotlinFunction, KotlinProperty};
use std::sync::{Mutex, OnceLock};

static TYPE_PARAMETER_IDENTITIES: OnceLock<
    Mutex<HashMap<(KlibPublicIdSignature, usize), &'static str>>,
> = OnceLock::new();

pub(super) fn type_parameter_identities(
    signature: &KlibPublicIdSignature,
    formals: &[crate::metadata::semantic::KotlinTypeParameter],
    enclosing: Option<&TypeParameterIdentities>,
) -> TypeParameterIdentities {
    let mut identities = TYPE_PARAMETER_IDENTITIES
        .get_or_init(|| Mutex::new(HashMap::default()))
        .lock()
        .unwrap();
    TypeParameterIdentities::from_provider_with_enclosing(
        formals,
        enclosing,
        |ordinal, parameter| {
            *identities
                .entry((signature.clone(), ordinal))
                .or_insert_with(|| crate::types::metadata_type_parameter(&parameter.name))
        },
    )
}

/// A function joined with the identity its library serialized it under and its validated
/// parameter identities.
pub(super) struct SignedFunction {
    pub(super) declaration: KotlinFunction,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) parameters: FunctionParameterIdentities,
    pub(super) type_parameters: TypeParameterIdentities,
    /// Compilation input assembly identifies the containing KLIB as the selected platform stdlib,
    /// whose compiler-owned declarations may receive package-level intrinsic roles. Ordinary
    /// dependencies remain false even when their manifests and packages mimic that stdlib.
    pub(super) compiler_builtin_owner: bool,
    /// Target-stdlib intrinsic semantics authenticated while the archive authority and complete
    /// metadata declaration are still joined.
    pub(super) target_intrinsic: Option<super::target_intrinsics::KlibIntrinsicShape>,
    /// Exact C target authenticated from an interop archive and this declaration's qualified
    /// annotation application.
    pub(super) direct_c_function: Option<crate::libraries::DirectCFunction>,
    /// This declaration's KLIB is a friend of the current compilation. Friendship is attached to
    /// the declaration while its archive identity is still known; package ownership cannot encode
    /// it because unrelated KLIBs may contribute declarations to the same package.
    pub(super) internal_friend: bool,
    /// Exact declaring KLIB module identity, when this declaration came from an archive.
    pub(super) module: Option<String>,
    /// The constant default of each value parameter, read from the library's IR; empty when the
    /// IR was not read or no default is a constant.
    pub(super) defaults: Vec<Option<crate::libraries::DefaultValue>>,
    /// Structural inline body authenticated from the same module-qualified IR declaration.
    pub(super) inline_body_plan: Option<Box<crate::libraries::InlineBodyPlan>>,
}

/// A property joined with the identities its library serialized it and its accessors under, and
/// its validated parameter identities.
pub(super) struct SignedProperty {
    /// Archive authority retained before an accessor's exact external identity is published.
    pub(super) compiler_builtin_owner: bool,
    pub(super) declaration: KotlinProperty,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) getter: KlibAccessorIdSignature,
    /// Present exactly for a `var`.
    pub(super) setter: Option<KlibAccessorIdSignature>,
    pub(super) parameters: PropertyParameterIdentities,
    pub(super) type_parameters: TypeParameterIdentities,
    /// Whether the KLIB that declares this property is a friend of this compilation.
    pub(super) internal_friend: bool,
    /// Exact declaring KLIB module identity, when this declaration came from an archive.
    pub(super) module: Option<String>,
}

/// A declaration the provider cannot publish, because its identity or its parameter identities
/// cannot be computed from its metadata. Publishing it without them would leave a selected call
/// with no IR body to join, or with parameters of no known source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibLibraryError {
    package: Vec<String>,
    declaration: String,
    detail: String,
}

impl KlibLibraryError {
    pub fn package(&self) -> &[String] {
        &self.package
    }

    pub fn declaration(&self) -> &str {
        &self.declaration
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for KlibLibraryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cannot sign KLIB declaration {} in package `{}`: {}",
            self.declaration,
            self.package.join("."),
            self.detail
        )
    }
}

impl std::error::Error for KlibLibraryError {}

/// Sign one top-level function of the package with exactly these segments and validate its
/// parameter identities.
pub(super) fn sign_package_function(
    package: &[String],
    declaration: KotlinFunction,
    internal_friend: bool,
    module: Option<&str>,
    native_interop_library: bool,
) -> Result<SignedFunction, KlibLibraryError> {
    let path = declaration.name.clone();
    let mut signed = sign_function(
        top_level(package, native_interop_library),
        &path,
        declaration,
        None,
        internal_friend,
    )?;
    signed.module = module.map(str::to_owned);
    Ok(signed)
}

/// Sign one top-level property of the package with exactly these segments, together with its
/// accessors, and validate its parameter identities and compile-time value.
pub(super) fn sign_package_property(
    package: &[String],
    declaration: KotlinProperty,
    internal_friend: bool,
    module: Option<&str>,
    native_interop_library: bool,
) -> Result<SignedProperty, KlibLibraryError> {
    let path = declaration.name.clone();
    let mut signed = sign_property(
        top_level(package, native_interop_library),
        &path,
        declaration,
        None,
        internal_friend,
    )?;
    signed.module = module.map(str::to_owned);
    Ok(signed)
}

fn top_level(package: &[String], native_interop_library: bool) -> MetadataContainer<'_> {
    MetadataContainer {
        package,
        classes: &[],
        native_interop_library,
    }
}

/// Sign one function in `container`, reported as `path` when it cannot be signed.
///
/// Metadata marks static exactly the declarations named through a classifier with no value
/// operand: a `companion { … }` block member and a companion extension. A companion extension's
/// written receiver names its classifier and is no parameter of its body.
pub(super) fn sign_function(
    container: MetadataContainer<'_>,
    path: &str,
    declaration: KotlinFunction,
    enclosing: Option<&TypeParameterIdentities>,
    internal_friend: bool,
) -> Result<SignedFunction, KlibLibraryError> {
    let package = container.package;
    let signature = package_function_signature(container, &declaration)
        .map_err(|error| unsignable(package, path, error))?;
    let parameters = if declaration.is_static {
        associated_function_parameter_identities(&declaration)
    } else {
        function_parameter_identities(&declaration)
    }
    .map_err(|error| unsignable(package, path, error))?;
    let type_parameters = type_parameter_identities(&signature, &declaration.formals, enclosing);
    Ok(SignedFunction {
        declaration,
        signature,
        parameters,
        type_parameters,
        compiler_builtin_owner: false,
        target_intrinsic: None,
        direct_c_function: None,
        internal_friend,
        module: None,
        defaults: Vec::new(),
        inline_body_plan: None,
    })
}

/// Sign one property in `container` together with its accessors, reported as `path` when it
/// cannot be signed. As for functions, a static property's written receiver is no parameter.
pub(super) fn sign_property(
    container: MetadataContainer<'_>,
    path: &str,
    declaration: KotlinProperty,
    enclosing: Option<&TypeParameterIdentities>,
    internal_friend: bool,
) -> Result<SignedProperty, KlibLibraryError> {
    let package = container.package;
    let fail = |error: &dyn std::fmt::Display| unsignable(package, path, error);
    let signature =
        package_property_signature(container, &declaration).map_err(|error| fail(&error))?;
    let getter =
        package_property_accessor_signature(container, &declaration, MetadataAccessor::Getter)
            .map_err(|error| fail(&error))?;
    let setter = declaration
        .is_var
        .then(|| {
            package_property_accessor_signature(container, &declaration, MetadataAccessor::Setter)
        })
        .transpose()
        .map_err(|error| fail(&error))?;
    let parameters = if declaration.is_static {
        associated_property_parameter_identities(&declaration)
    } else {
        property_parameter_identities(&declaration)
    }
    .map_err(|error| fail(&error))?;
    let type_parameters = type_parameter_identities(&signature, &declaration.formals, enclosing);
    // A constant read is folded at its use site, so a `const val` without its value would leave
    // the read with nothing to fold.
    if declaration.is_const && declaration.constant.is_none() {
        return Err(fail(&format!(
            "const property {} has no compile-time value",
            declaration.name
        )));
    }
    Ok(SignedProperty {
        compiler_builtin_owner: false,
        declaration,
        signature,
        getter,
        setter,
        parameters,
        type_parameters,
        internal_friend,
        module: None,
    })
}

pub(super) fn unsignable(
    package: &[String],
    declaration: &str,
    error: impl std::fmt::Display,
) -> KlibLibraryError {
    KlibLibraryError {
        package: package.to_vec(),
        declaration: declaration.to_string(),
        detail: error.to_string(),
    }
}
