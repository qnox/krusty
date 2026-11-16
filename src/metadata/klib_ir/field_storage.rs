//! Typed storage flags from Kotlin's serialized `IrFlags` schemas of fields and properties.

use super::tree::{KlibIrField, KlibIrProperty};
use crate::types::Visibility;

pub(crate) struct InstanceStorageFlags {
    pub(crate) is_final: bool,
    pub(crate) visibility: Visibility,
}

/// HAS_ANNOTATIONS (0), VISIBILITY (1..3), then IS_FINAL (4), EXTERNAL (5), STATIC (6). This is
/// the field schema, not the constructor/property flag word.
const STATIC: u64 = 1 << 6;

impl KlibIrField {
    pub(crate) fn instance_storage_flags(&self) -> Option<InstanceStorageFlags> {
        storage_flags(self.base.flags)
    }

    /// The flags of a static field, such as a top-level property's backing field. An external
    /// field or an unknown flag has none.
    pub(crate) fn static_storage_flags(&self) -> Option<InstanceStorageFlags> {
        let flags = self.base.flags;
        if flags & STATIC == 0 {
            return None;
        }
        storage_flags(flags & !STATIC)
    }
}

impl KlibIrProperty {
    /// Whether the property is `lateinit`: its backing field starts unset, and a read of it
    /// before the first write throws.
    pub(crate) fn is_lateinit(&self) -> bool {
        // A serialized property's flag word has the layout of Kotlin metadata's property flags.
        self.base.flags & crate::metadata::property_flags::IS_LATEINIT != 0
    }
}

fn storage_flags(flags: u64) -> Option<InstanceStorageFlags> {
    if flags & !0b1_1111 != 0 || (flags >> 1) & 7 > 5 {
        return None;
    }
    Some(InstanceStorageFlags {
        is_final: flags & (1 << 4) != 0,
        visibility: crate::metadata::semantic::declaration_visibility(flags),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::tree::{KlibIrDeclarationBase, KlibIrTypeId};
    use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};

    fn field(flags: u64) -> KlibIrField {
        KlibIrField {
            base: KlibIrDeclarationBase {
                symbol: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Field,
                    signature: KlibIrSignature::FileLocal { file: 0, slot: 0 },
                },
                flags,
                origin: "PROPERTY_BACKING_FIELD".to_owned(),
                annotations: Vec::new(),
            },
            name: "stored".to_owned(),
            ty: KlibIrTypeId(0),
            initializer: None,
        }
    }

    #[test]
    fn instance_field_flags_keep_their_own_finality_and_visibility_schema() {
        let flags = field((1 << 1) | (1 << 4)).instance_storage_flags().unwrap();
        assert!(flags.is_final);
        assert_eq!(flags.visibility, Visibility::Private);
        let flags = field(3 << 1).instance_storage_flags().unwrap();
        assert!(!flags.is_final);
        assert_eq!(flags.visibility, Visibility::Public);
    }

    #[test]
    fn non_instance_and_unknown_field_flags_grant_no_storage_capability() {
        for flags in [1 << 5, 1 << 6, 1 << 19, 6 << 1, 7 << 1] {
            assert!(field(flags).instance_storage_flags().is_none(), "{flags}");
        }
    }

    #[test]
    fn static_field_flags_require_the_static_bit_and_keep_the_storage_schema() {
        let flags = field((1 << 6) | (1 << 1)).static_storage_flags().unwrap();
        assert!(!flags.is_final);
        assert_eq!(flags.visibility, Visibility::Private);
        assert!(
            field((1 << 6) | (1 << 4))
                .static_storage_flags()
                .unwrap()
                .is_final
        );
        for flags in [1 << 1, (1 << 6) | (1 << 5), (1 << 6) | (1 << 19)] {
            assert!(field(flags).static_storage_flags().is_none(), "{flags}");
        }
    }
}
