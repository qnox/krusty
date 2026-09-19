//! Fallible metadata wire decoders shared by target adapters.

mod klib;
mod type_shape;
mod wire;

pub use klib::PackageFragmentDecodeError;
pub(crate) use klib::{
    decode_package_fragment, field, parse_module_header, require_wire, strip_builtins_header,
    Cursor, DecodedPackageFragment, QName,
};
pub(crate) use type_shape::{parse_type_node, ParsedProjection, ParsedTypeArgument};
pub(crate) use wire::Pb;
