//! Target runtime and ABI services used after frontend checking.

use crate::libraries::LibraryCallable;
use crate::types::Ty;

/// Platform-provided accessor used by counted range/progression loop lowering. The name and descriptor
/// are backend tokens; common lowering only emits them back to the same backend.
#[derive(Clone, Debug)]
pub struct PlatformAccessor {
    pub name: String,
    pub descriptor: String,
}

/// Platform construction plan for a Kotlin range value expression. `elem` is the semantic element
/// type operands coerce to; `through` constructs `a..b`; `until` realizes `a..<b` when supported.
#[derive(Clone, Debug)]
pub struct RangeConstruction {
    pub elem: Ty,
    pub result: Ty,
    pub through: PlatformRangeCtor,
    pub until: Option<LibraryCallable>,
    pub through_static: Option<LibraryCallable>,
}

/// Platform-owned range constructor tokens. `trailing_nulls` covers synthetic marker arguments such as
/// JVM unsigned range constructors without exposing those marker classes to common lowering.
#[derive(Clone, Debug)]
pub struct PlatformRangeCtor {
    pub internal: String,
    pub ctor_desc: String,
    pub trailing_nulls: usize,
}
