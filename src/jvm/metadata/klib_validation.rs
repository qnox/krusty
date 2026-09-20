//! JVM semantic adaptation for checked KLIB metadata fragments.
//!
//! Wire/schema validation belongs to [`crate::metadata::decode`]. This module retains the legacy
//! `Builtin*` semantic adapter until its declarations move to the common metadata model.

use super::BuiltinPackage;
use crate::metadata::decode::{
    decode_package_fragment, field, parse_type_node, parse_type_param, parse_value_parameter,
    require_wire, Cursor, DecodedPackageFragment, ParsedTypeArgument, ParsedVariance, QName,
};

pub(crate) use crate::metadata::decode::{parse_module_header, PackageFragmentDecodeError};

mod semantic;

/// Decode one dependency-owned fragment atomically. Structural and semantic parsing are both
/// fallible: a nested declaration is published only after all of its names, types, parameters and
/// table references have resolved successfully.
pub(crate) fn parse_package_fragment_checked(
    bytes: &[u8],
) -> Result<BuiltinPackage, PackageFragmentDecodeError> {
    semantic::parse(decode_package_fragment(bytes)?)
}
