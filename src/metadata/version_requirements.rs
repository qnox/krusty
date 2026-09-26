//! `VersionRequirement` records: which compiler or language version a reader must have to consume a
//! declaration. A class or package owns one `VersionRequirementTable` (f32); a declaration refers to
//! its entries by index (`version_requirement`, f31). Equal requirements share one entry, in the
//! order of their first use, as kotlinc's `MutableVersionRequirementTable` does.

use super::protobuf::Pb;

/// `ProtoBuf.VersionRequirement.VersionKind`. `LanguageVersion` is the proto default and omitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionKind {
    LanguageVersion = 0,
    CompilerVersion = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionRequirement {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
    pub kind: VersionKind,
}

impl VersionRequirement {
    pub const fn compiler(major: u8, minor: u8, patch: u8) -> Self {
        VersionRequirement {
            major,
            minor,
            patch,
            kind: VersionKind::CompilerVersion,
        }
    }

    fn encode(self) -> Pb {
        let VersionRequirement {
            major,
            minor,
            patch,
            kind,
        } = self;
        let mut requirement = Pb::new();
        // The compact word is `major:3 | minor:4 | patch:7`; a larger component uses `version_full`.
        if major <= 7 && minor <= 15 && patch <= 127 {
            requirement.field_varint(
                1,
                u64::from(major) | (u64::from(minor) << 3) | (u64::from(patch) << 7),
            );
        } else {
            requirement.field_varint(
                2,
                u64::from(major) | (u64::from(minor) << 8) | (u64::from(patch) << 16),
            );
        }
        if kind != VersionKind::LanguageVersion {
            requirement.field_varint(6, kind as u64);
        }
        requirement
    }
}

/// Since Kotlin 1.4 an inline function checks its parameters with `Intrinsics.checkNotNullParameter`,
/// which compilers older than 1.3.50 cannot inline through a functional parameter.
pub const INLINE_PARAMETER_NULL_CHECK: VersionRequirement = VersionRequirement::compiler(1, 3, 50);

/// kotlinc's `FirJvmSerializerExtension.needsInlineParameterNullCheckRequirement`: a named, non-private,
/// non-suspend inline function with a value parameter or extension receiver of a function type
/// (`has_function_typed_parameter`, the checker's fact), when parameter assertions are generated.
/// Property accessors never qualify.
pub fn needs_inline_parameter_null_check(
    function_flags: u64,
    has_function_typed_parameter: bool,
    param_assertions: bool,
) -> bool {
    use super::function_flags::{IS_INLINE, IS_SUSPEND, VISIBILITY_MASK, VISIBILITY_PRIVATE};
    param_assertions
        && has_function_typed_parameter
        && function_flags & IS_INLINE != 0
        && function_flags & IS_SUSPEND == 0
        && function_flags & VISIBILITY_MASK != VISIBILITY_PRIVATE
}

#[derive(Default)]
pub struct VersionRequirementTable {
    entries: Vec<VersionRequirement>,
}

impl VersionRequirementTable {
    /// The table index of `requirement`, appending it on first use.
    pub fn index(&mut self, requirement: VersionRequirement) -> u64 {
        let index = match self.entries.iter().position(|&entry| entry == requirement) {
            Some(index) => index,
            None => {
                self.entries.push(requirement);
                self.entries.len() - 1
            }
        };
        index as u64
    }

    /// The `VersionRequirementTable` message, or `None` when no declaration required anything.
    pub fn encode(&self) -> Option<Pb> {
        (!self.entries.is_empty()).then(|| {
            let mut table = Pb::new();
            for &entry in &self.entries {
                table.repeated_message(1, &entry.encode());
            }
            table
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::function_flags::IS_INLINE;

    const PUBLIC_FINAL: u64 = 6;

    #[test]
    fn disabled_parameter_assertions_drop_the_requirement() {
        let inline = PUBLIC_FINAL | IS_INLINE;
        assert!(needs_inline_parameter_null_check(inline, true, true));
        assert!(!needs_inline_parameter_null_check(inline, true, false));
        assert!(!needs_inline_parameter_null_check(inline, false, true));
    }

    /// kotlinc 2.4.20 writes `VersionRequirementTable { requirement { version: 6425, version_kind:
    /// COMPILER_VERSION } }` for the inline null-check requirement, once however many use it.
    #[test]
    fn equal_requirements_share_one_entry() {
        let mut table = VersionRequirementTable::default();
        assert_eq!(table.index(INLINE_PARAMETER_NULL_CHECK), 0);
        assert_eq!(table.index(VersionRequirement::compiler(1, 4, 0)), 1);
        assert_eq!(table.index(INLINE_PARAMETER_NULL_CHECK), 0);
        let bytes = table.encode().expect("a used table encodes").into_bytes();
        assert_eq!(
            bytes,
            [0x0a, 0x05, 0x08, 0x99, 0x32, 0x30, 0x01, 0x0a, 0x04, 0x08, 0x21, 0x30, 0x01]
        );
    }
}
