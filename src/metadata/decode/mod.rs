//! Fallible metadata wire decoders shared by target adapters.

mod klib;

pub(crate) use klib::{
    decode_package_fragment, field, parse_module_header, require_wire, strip_builtins_header,
    Cursor, DecodedPackageFragment, PackageFragmentDecodeError, QName,
};
