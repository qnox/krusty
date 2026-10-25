//! Typed field flags from Kotlin's serialized `IrFlags.getFieldFlags` schema.

use super::tree::KlibIrField;
use crate::types::Visibility;

pub(crate) struct InstanceStorageFlags {
    pub(crate) is_final: bool,
    pub(crate) visibility: Visibility,
}

impl KlibIrField {
    pub(crate) fn instance_storage_flags(&self) -> Option<InstanceStorageFlags> {
        // HAS_ANNOTATIONS (0), VISIBILITY (1..3), then IS_FINAL (4), EXTERNAL (5), STATIC (6).
        // This is the field schema, not the constructor/property flag word.
        let flags = self.base.flags;
        if flags & !0b1_1111 != 0 || (flags >> 1) & 7 > 5 {
            return None;
        }
        Some(InstanceStorageFlags {
            is_final: flags & (1 << 4) != 0,
            visibility: crate::metadata::semantic::declaration_visibility(flags),
        })
    }
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
}
