//! Symbol references inside serialized KLIB IR.
//!
//! A serialized symbol is one `int64`: the low byte is its kind and the rest indexes the file's
//! signature table. The signature says WHICH declaration the symbol denotes. A public declaration
//! carries its exact `CommonIdSignature`; a property accessor carries its property's identity plus
//! its own name; everything else (file-private, scoped, local and composite signatures) is only
//! meaningful inside the file that serialized it, so it is kept as that file's signature slot and
//! never matched to a declaration by spelling.

use super::wire::{message, unique_bytes, unique_varint};
use super::KlibIrDecodeError;
use crate::metadata::id_signature::{decode_public_id_signature, KlibPublicIdSignature};

const ENTRY: &str = "signatures.knt";

/// `BinarySymbolData.SymbolKind`, in serialized ordinal order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KlibIrSymbolKind {
    Function,
    Constructor,
    EnumEntry,
    Field,
    ValueParameter,
    ReturnableBlock,
    Class,
    TypeParameter,
    Variable,
    AnonymousInit,
    StandaloneField,
    ReceiverParameter,
    Property,
    LocalDelegatedProperty,
    TypeAlias,
    File,
}

impl KlibIrSymbolKind {
    const ALL: [Self; 16] = [
        Self::Function,
        Self::Constructor,
        Self::EnumEntry,
        Self::Field,
        Self::ValueParameter,
        Self::ReturnableBlock,
        Self::Class,
        Self::TypeParameter,
        Self::Variable,
        Self::AnonymousInit,
        Self::StandaloneField,
        Self::ReceiverParameter,
        Self::Property,
        Self::LocalDelegatedProperty,
        Self::TypeAlias,
        Self::File,
    ];
}

/// Which declaration a symbol denotes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum KlibIrSignature {
    /// A public declaration, exactly as every module that links against it names it.
    Public(KlibPublicIdSignature),
    /// A getter or setter of a public property: the property's identity and the accessor's name
    /// (`<get-size>`), plus the serialized accessor hash and mask.
    Accessor {
        property: KlibPublicIdSignature,
        name: String,
        hash: u64,
        mask: u64,
    },
    /// A declaration whose identity is private to one serialized file: file index within the
    /// KLIB and slot in that file's signature table.
    FileLocal { file: u32, slot: u32 },
}

/// One decoded symbol reference.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibIrSymbol {
    pub kind: KlibIrSymbolKind,
    pub signature: KlibIrSignature,
}

/// One file's signature table, decoded on demand and memoized by slot.
pub(super) struct SignatureTable<'a> {
    file: u32,
    entries: &'a [&'a [u8]],
    strings: &'a [String],
    decoded: Vec<Option<KlibIrSignature>>,
}

impl<'a> SignatureTable<'a> {
    pub(super) fn new(file: u32, entries: &'a [&'a [u8]], strings: &'a [String]) -> Self {
        Self {
            file,
            entries,
            strings,
            decoded: vec![None; entries.len()],
        }
    }

    pub(super) fn symbol(&mut self, code: u64) -> Result<KlibIrSymbol, KlibIrDecodeError> {
        let kind_id = usize::try_from(code & 0xff).expect("a byte fits");
        let kind = *KlibIrSymbolKind::ALL.get(kind_id).ok_or_else(|| {
            KlibIrDecodeError::malformed(ENTRY, 0, format!("unknown symbol kind {kind_id}"))
        })?;
        let slot = self.slot(code >> 8, "symbol")?;
        Ok(KlibIrSymbol {
            kind,
            signature: self.signature(slot)?,
        })
    }

    fn slot(&self, raw: u64, context: &str) -> Result<usize, KlibIrDecodeError> {
        usize::try_from(raw)
            .ok()
            .filter(|slot| *slot < self.entries.len())
            .ok_or_else(|| {
                KlibIrDecodeError::malformed(
                    ENTRY,
                    0,
                    format!("{context} references absent signature {raw}"),
                )
            })
    }

    fn signature(&mut self, slot: usize) -> Result<KlibIrSignature, KlibIrDecodeError> {
        if let Some(signature) = &self.decoded[slot] {
            return Ok(signature.clone());
        }
        let signature = self.decode(slot)?;
        self.decoded[slot] = Some(signature.clone());
        Ok(signature)
    }

    fn decode(&mut self, slot: usize) -> Result<KlibIrSignature, KlibIrDecodeError> {
        let bytes = self.entries[slot];
        if let Some(public) = decode_public_id_signature(bytes, self.strings).map_err(|error| {
            KlibIrDecodeError::malformed(
                ENTRY,
                error.offset(),
                format!("signature {slot}: {}", error.detail()),
            )
        })? {
            return Ok(KlibIrSignature::Public(public));
        }
        let fields = message(bytes, ENTRY, 0)?;
        // IdSignature.accessor_sig; the public decoder above already proved exactly one kind.
        if let Some(accessor) = unique_bytes(&fields, 3, "accessor signature", ENTRY)? {
            let accessor = message(accessor.value, ENTRY, accessor.value_offset)?;
            let property = unique_varint(&accessor, 1, "accessor property", ENTRY)?;
            let name = unique_varint(&accessor, 2, "accessor name", ENTRY)?;
            let hash = unique_varint(&accessor, 3, "accessor hash", ENTRY)?;
            let mask = unique_varint(&accessor, 4, "accessor flags", ENTRY)?.unwrap_or(0);
            let (Some(property), Some(name), Some(hash)) = (property, name, hash) else {
                return Err(KlibIrDecodeError::malformed(
                    ENTRY,
                    0,
                    format!("accessor signature {slot} misses a required field"),
                ));
            };
            let property_slot = self.slot(property, "accessor property")?;
            if let KlibIrSignature::Public(property) = self.signature(property_slot)? {
                let name = string(self.strings, name, "accessor name")?;
                return Ok(KlibIrSignature::Accessor {
                    property,
                    name,
                    hash,
                    mask,
                });
            }
        }
        Ok(KlibIrSignature::FileLocal {
            file: self.file,
            slot: u32::try_from(slot).expect("a table slot fits u32"),
        })
    }
}

pub(super) fn string(
    strings: &[String],
    index: u64,
    context: &str,
) -> Result<String, KlibIrDecodeError> {
    usize::try_from(index)
        .ok()
        .and_then(|index| strings.get(index))
        .cloned()
        .ok_or_else(|| {
            KlibIrDecodeError::malformed(
                "strings.knt",
                0,
                format!("{context} references absent string {index}"),
            )
        })
}
