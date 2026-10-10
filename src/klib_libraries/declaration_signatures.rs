//! Exact public `IdSignature`s of the declarations a KLIB provider publishes.

use crate::libraries::{
    function_parameter_identities, property_parameter_identities, FunctionParameterIdentities,
    PropertyParameterIdentities,
};
use crate::metadata::id_signature::{
    package_function_signature, package_property_accessor_signature, package_property_signature,
    KlibAccessorIdSignature, KlibPublicIdSignature, MetadataAccessor, MetadataContainer,
};
use crate::metadata::semantic::{KotlinFunction, KotlinProperty};

/// A top-level function joined with the identity its library serialized it under and its
/// validated parameter identities.
pub(super) struct SignedFunction {
    pub(super) declaration: KotlinFunction,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) parameters: FunctionParameterIdentities,
}

/// A top-level property joined with the identities its library serialized it and its accessors
/// under, and its validated parameter identities.
pub(super) struct SignedProperty {
    pub(super) declaration: KotlinProperty,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) getter: KlibAccessorIdSignature,
    /// Present exactly for a `var`.
    pub(super) setter: Option<KlibAccessorIdSignature>,
    pub(super) parameters: PropertyParameterIdentities,
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
) -> Result<SignedFunction, KlibLibraryError> {
    let container = MetadataContainer {
        package,
        classes: &[],
        native_interop_library: false,
    };
    let signature = package_function_signature(container, &declaration)
        .map_err(|error| unsignable(package, &declaration.name, error))?;
    let parameters = function_parameter_identities(&declaration)
        .map_err(|error| unsignable(package, &declaration.name, error))?;
    Ok(SignedFunction {
        declaration,
        signature,
        parameters,
    })
}

/// Sign one top-level property of the package with exactly these segments, together with its
/// accessors, and validate its parameter identities and compile-time value.
pub(super) fn sign_package_property(
    package: &[String],
    declaration: KotlinProperty,
) -> Result<SignedProperty, KlibLibraryError> {
    let container = MetadataContainer {
        package,
        classes: &[],
        native_interop_library: false,
    };
    let fail = |error: &dyn std::fmt::Display| unsignable(package, &declaration.name, error);
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
    let parameters = property_parameter_identities(&declaration).map_err(|error| fail(&error))?;
    // A constant read is folded at its use site, so a `const val` without its value would leave
    // the read with nothing to fold.
    if declaration.is_const && declaration.constant.is_none() {
        return Err(fail(&format!(
            "const property {} has no compile-time value",
            declaration.name
        )));
    }
    Ok(SignedProperty {
        declaration,
        signature,
        getter,
        setter,
        parameters,
    })
}

fn unsignable(
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
