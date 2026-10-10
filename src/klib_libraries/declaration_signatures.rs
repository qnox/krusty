//! Exact public `IdSignature`s of the declarations a KLIB provider publishes.

use crate::libraries::{function_parameter_identities, FunctionParameterIdentities};
use crate::metadata::id_signature::{
    package_function_signature, KlibPublicIdSignature, MetadataContainer,
};
use crate::metadata::semantic::KotlinFunction;

/// A top-level function joined with the identity its library serialized it under and its
/// validated parameter identities.
pub(super) struct SignedFunction {
    pub(super) declaration: KotlinFunction,
    pub(super) signature: KlibPublicIdSignature,
    pub(super) parameters: FunctionParameterIdentities,
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
