//! Symbol references inside serialized KLIB IR.
//!
//! A serialized symbol is one `int64`: the low byte is its kind and the rest indexes the file's
//! signature table. The signature says WHICH declaration the symbol denotes. A public declaration
//! carries its exact `CommonIdSignature`; a property accessor carries its property's identity plus
//! its own name; everything else (file-private, scoped, local and composite signatures) is only
//! meaningful inside the file that serialized it, so it is kept as that file's signature slot and
//! never matched to a declaration by spelling. Kotlin's prepared-inline component is the one
//! exception: its private copy wraps a public-form signature in an exact
//! `Composite(File, Public)` identity. That component projects the serialized inner identity for
//! references inside the copied body; the declaration table separately owns the original public
//! join key. A type parameter of a public declaration, `Composite(Public, Local(<TP>, index))`, is
//! that owner and index in every module. Other IR composites remain file-local.

use super::wire::{message, unique_bytes, unique_varint};
use super::KlibIrDecodeError;
use crate::metadata::id_signature::{
    decode_public_id_signature, KlibAccessorIdSignature, KlibPublicIdSignature,
};

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
    /// A getter or setter of a public property.
    Accessor(KlibAccessorIdSignature),
    /// A declaration whose identity is private to one serialized file: file index within the
    /// KLIB and slot in that file's signature table.
    FileLocal { file: u32, slot: u32 },
    /// The type parameter at `index` of the public declaration `owner`: Kotlin's
    /// `Composite(owner, Local(<TP>, index))`. Every module that links against the owner names
    /// it alike, so a body may reference one its own declarations do not declare.
    TypeParameter {
        owner: KlibPublicIdSignature,
        index: u32,
    },
}

/// One decoded symbol reference.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibIrSymbol {
    pub kind: KlibIrSymbolKind,
    pub signature: KlibIrSignature,
}

/// What a serialized file-local signature (`FileLocalIdSignature`) records beside its table slot:
/// the signature of its container and its id. kotlinc numbers an ordinary file-local declaration
/// by a counter, but names a fake override of a local class by the hash of its own mangled
/// signature (`FileLocalIdSignatureComputer.stableIndexForFakeOverride`), which is what links such a
/// member from a call site that no declaration of the library defines.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KlibFileLocalOrigin {
    pub container: KlibIrSignature,
    pub id: i64,
}

/// One file's signature table, decoded on demand and memoized by slot.
pub(super) struct SignatureTable<'a> {
    file: u32,
    entries: &'a [&'a [u8]],
    strings: &'a [String],
    decoded: Vec<Option<KlibIrSignature>>,
    prepared_inline: bool,
    origins: std::collections::HashMap<KlibIrSignature, KlibFileLocalOrigin>,
}

impl<'a> SignatureTable<'a> {
    pub(super) fn new(file: u32, entries: &'a [&'a [u8]], strings: &'a [String]) -> Self {
        Self::with_component(file, entries, strings, false)
    }

    /// A signature table from Kotlin's prepared-inline component.
    pub(super) fn prepared_inline(
        file: u32,
        entries: &'a [&'a [u8]],
        strings: &'a [String],
    ) -> Self {
        Self::with_component(file, entries, strings, true)
    }

    fn with_component(
        file: u32,
        entries: &'a [&'a [u8]],
        strings: &'a [String],
        prepared_inline: bool,
    ) -> Self {
        Self {
            file,
            entries,
            strings,
            decoded: vec![None; entries.len()],
            prepared_inline,
            origins: std::collections::HashMap::new(),
        }
    }

    /// The origins of the file-local signatures decoded so far.
    pub(super) fn take_origins(
        &mut self,
    ) -> std::collections::HashMap<KlibIrSignature, KlibFileLocalOrigin> {
        std::mem::take(&mut self.origins)
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

    /// The identity used as one `irDeclarations.knd` table key. Prepared components key a private
    /// file-scoped copy by the original public declaration identity; that key, rather than the
    /// copied function's own symbol, is the cross-component join.
    pub(super) fn declaration_identity(
        &mut self,
        raw: u64,
    ) -> Result<KlibIrSignature, KlibIrDecodeError> {
        let slot = self.slot(raw, "declaration table")?;
        self.signature(slot)
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
                return Ok(KlibIrSignature::Accessor(KlibAccessorIdSignature::new(
                    property, name, hash, mask,
                )));
            }
        }
        // Kotlin 2.4's prepared-inline component gives its private copied function a
        // `Composite(File(<dummy>), Public(copy))` signature. Project the exact inner identity only
        // for references inside this component; `irDeclarations.knd` independently carries the
        // original public declaration key used across components. General composites in ordinary
        // IR stay file-local, and neither path reconstructs an identity from a spelling.
        if self.prepared_inline {
            if let Some(composite) = unique_bytes(&fields, 5, "composite signature", ENTRY)? {
                let composite_offset = composite.value_offset;
                let composite = message(composite.value, ENTRY, composite_offset)?;
                let container =
                    unique_varint(&composite, 1, "composite signature container", ENTRY)?;
                let inner = unique_varint(&composite, 2, "composite inner signature", ENTRY)?;
                let (Some(container), Some(inner)) = (container, inner) else {
                    return Err(KlibIrDecodeError::malformed(
                        ENTRY,
                        composite_offset,
                        format!("composite signature {slot} misses a required field"),
                    ));
                };
                let container = self.slot(container, "composite signature container")?;
                let inner = self.slot(inner, "composite inner signature")?;
                if self.is_file_signature(container)? {
                    if let Some(public) = self.direct_public_signature(inner)? {
                        return Ok(KlibIrSignature::Public(public));
                    }
                }
            }
        }
        if let Some((owner, index)) = self.public_type_parameter(&fields)? {
            return Ok(KlibIrSignature::TypeParameter { owner, index });
        }
        let local = KlibIrSignature::FileLocal {
            file: self.file,
            slot: u32::try_from(slot).expect("a table slot fits u32"),
        };
        if let Some(private) = unique_bytes(&fields, 2, "file-local signature", ENTRY)? {
            let private = message(private.value, ENTRY, private.value_offset)?;
            let container = unique_varint(&private, 1, "file-local container", ENTRY)?;
            let id = unique_varint(&private, 2, "file-local id", ENTRY)?;
            if let (Some(container), Some(id)) = (container, id) {
                let container = self.slot(container, "file-local container")?;
                // A signature of a declaration the file serializes never contains itself.
                if container != slot {
                    let container = self.signature(container)?;
                    self.origins.insert(
                        local.clone(),
                        KlibFileLocalOrigin {
                            container,
                            id: id as i64,
                        },
                    );
                }
            }
        }
        Ok(local)
    }

    /// `Composite(Public(owner), Local(<TP>, index))`, the identity Kotlin gives the type
    /// parameter `index` of a public declaration. A composite of any other shape stays file-local.
    fn public_type_parameter(
        &self,
        fields: &[super::wire::WireField<'_>],
    ) -> Result<Option<(KlibPublicIdSignature, u32)>, KlibIrDecodeError> {
        let Some(composite) = unique_bytes(fields, 5, "composite signature", ENTRY)? else {
            return Ok(None);
        };
        let composite_offset = composite.value_offset;
        let composite = message(composite.value, ENTRY, composite_offset)?;
        let container = unique_varint(&composite, 1, "composite signature container", ENTRY)?;
        let inner = unique_varint(&composite, 2, "composite inner signature", ENTRY)?;
        let (Some(container), Some(inner)) = (container, inner) else {
            return Ok(None);
        };
        let container = self.slot(container, "composite signature container")?;
        let inner = self.slot(inner, "composite inner signature")?;
        let Some(owner) = self.direct_public_signature(container)? else {
            return Ok(None);
        };
        let inner_fields = message(self.entries[inner], ENTRY, 0)?;
        // IdSignature.local_sig: LocalSignature { local_fq_name, local_hash, debug_info }.
        let Some(local) = unique_bytes(&inner_fields, 6, "local signature", ENTRY)? else {
            return Ok(None);
        };
        let local = message(local.value, ENTRY, local.value_offset)?;
        let name = super::wire::packed_values(&local, 1, ENTRY, "local signature name")?;
        let [name] = name.as_slice() else {
            return Ok(None);
        };
        if string(self.strings, *name, "local signature name")? != "<TP>" {
            return Ok(None);
        }
        let Some(index) = unique_varint(&local, 2, "local signature hash", ENTRY)? else {
            return Ok(None);
        };
        let Ok(index) = u32::try_from(index) else {
            return Err(KlibIrDecodeError::malformed(
                ENTRY,
                composite_offset,
                format!("type parameter index {index} does not fit u32"),
            ));
        };
        Ok(Some((owner, index)))
    }

    fn direct_public_signature(
        &self,
        slot: usize,
    ) -> Result<Option<KlibPublicIdSignature>, KlibIrDecodeError> {
        decode_public_id_signature(self.entries[slot], self.strings).map_err(|error| {
            KlibIrDecodeError::malformed(
                ENTRY,
                error.offset(),
                format!("signature {slot}: {}", error.detail()),
            )
        })
    }

    fn is_file_signature(&self, slot: usize) -> Result<bool, KlibIrDecodeError> {
        if self.direct_public_signature(slot)?.is_some() {
            return Ok(false);
        }
        let fields = message(self.entries[slot], ENTRY, 0)?;
        Ok(unique_bytes(&fields, 7, "file signature", ENTRY)?.is_some())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            bytes.push(if value == 0 { byte } else { byte | 0x80 });
            if value == 0 {
                return bytes;
            }
        }
    }

    fn varint_field(into: &mut Vec<u8>, field: u64, value: u64) {
        into.extend(varint(field << 3));
        into.extend(varint(value));
    }

    fn bytes_field(into: &mut Vec<u8>, field: u64, value: &[u8]) {
        into.extend(varint(field << 3 | 2));
        into.extend(varint(value.len() as u64));
        into.extend(value);
    }

    fn fixed64_field(into: &mut Vec<u8>, field: u64, value: u64) {
        into.extend(varint(field << 3 | 1));
        into.extend(value.to_le_bytes());
    }

    fn public_signature() -> Vec<u8> {
        let mut common = Vec::new();
        bytes_field(&mut common, 1, &[0]);
        varint_field(&mut common, 2, 1);
        varint_field(&mut common, 5, 0);
        fixed64_field(&mut common, 6, 0x14ab_8f30_859f_59a9);
        let mut signature = Vec::new();
        bytes_field(&mut signature, 1, &common);
        signature
    }

    fn composite(container: u64, inner: u64) -> Vec<u8> {
        let mut body = Vec::new();
        varint_field(&mut body, 1, container);
        varint_field(&mut body, 2, inner);
        let mut signature = Vec::new();
        bytes_field(&mut signature, 5, &body);
        signature
    }

    /// `IdSignature.local_sig`: `LocalSignature { local_fq_name = [name], local_hash = hash }`.
    fn local(name: u64, hash: u64) -> Vec<u8> {
        let mut body = Vec::new();
        bytes_field(&mut body, 1, &varint(name));
        varint_field(&mut body, 2, hash);
        let mut signature = Vec::new();
        bytes_field(&mut signature, 6, &body);
        signature
    }

    #[test]
    fn a_public_declaration_type_parameter_is_its_owner_and_index_in_every_table() {
        let public = public_signature();
        let parameter = local(2, 1);
        let composite = composite(0, 1);
        let entries = [
            public.as_slice(),
            parameter.as_slice(),
            composite.as_slice(),
        ];
        let strings = ["fixture".to_owned(), "probe".to_owned(), "<TP>".to_owned()];
        for table in [
            SignatureTable::new(3, &entries, &strings),
            SignatureTable::prepared_inline(3, &entries, &strings),
        ] {
            let mut table = table;
            let symbol = table
                .symbol(2 << 8 | KlibIrSymbolKind::TypeParameter as u64)
                .expect("a type parameter symbol");
            let KlibIrSignature::TypeParameter { owner, index } = symbol.signature else {
                panic!("the owner's type parameter, not {:?}", symbol.signature)
            };
            assert_eq!(owner.declaration().segments(), ["probe"]);
            assert_eq!(owner.member_id(), Some(0x14ab_8f30_859f_59a9));
            assert_eq!(index, 1);
        }
    }

    #[test]
    fn another_local_name_under_a_public_owner_stays_file_local() {
        let public = public_signature();
        let parameter = local(2, 1);
        let composite = composite(0, 1);
        let entries = [
            public.as_slice(),
            parameter.as_slice(),
            composite.as_slice(),
        ];
        let strings = ["fixture".to_owned(), "probe".to_owned(), "<BF>".to_owned()];
        let symbol = SignatureTable::new(3, &entries, &strings)
            .symbol(2 << 8)
            .expect("a local symbol");
        assert_eq!(
            symbol.signature,
            KlibIrSignature::FileLocal { file: 3, slot: 2 }
        );
    }

    #[test]
    fn prepared_inline_file_wrapper_projects_its_exact_public_identity() {
        let file = [0x3a, 0x00];
        let public = public_signature();
        let composite = composite(0, 1);
        let entries = [file.as_slice(), public.as_slice(), composite.as_slice()];
        let strings = ["fixture".to_owned(), "probe".to_owned()];

        let symbol = SignatureTable::prepared_inline(4, &entries, &strings)
            .symbol(2 << 8)
            .expect("the prepared function has a valid symbol");
        let KlibIrSignature::Public(identity) = symbol.signature else {
            panic!("the exact prepared wrapper must publish its inner public identity")
        };
        assert_eq!(identity.package().segments(), ["fixture"]);
        assert_eq!(identity.declaration().segments(), ["probe"]);
        assert_eq!(identity.member_id(), Some(0x14ab_8f30_859f_59a9));
    }

    fn private_signature(container: u64, id: u64) -> Vec<u8> {
        let mut body = Vec::new();
        varint_field(&mut body, 1, container);
        varint_field(&mut body, 2, id);
        let mut signature = Vec::new();
        bytes_field(&mut signature, 2, &body);
        signature
    }

    #[test]
    fn a_file_local_signature_records_its_container_and_id() {
        // A fake override of a local class is file-local, named by its container and the hash of
        // its mangle (`-5` here is a counter-sized stand-in for such an id as a signed value).
        let public = public_signature();
        let property = private_signature(0, 11);
        let getter = private_signature(1, (-5_i64) as u64);
        let entries = [public.as_slice(), property.as_slice(), getter.as_slice()];
        let strings = ["fixture".to_owned(), "probe".to_owned()];

        let mut table = SignatureTable::new(7, &entries, &strings);
        let symbol = table.symbol(2 << 8).expect("the getter is a valid symbol");
        assert_eq!(
            symbol.signature,
            KlibIrSignature::FileLocal { file: 7, slot: 2 }
        );
        let origins = table.take_origins();
        let getter = origins
            .get(&KlibIrSignature::FileLocal { file: 7, slot: 2 })
            .expect("the getter records its origin");
        assert_eq!(getter.id, -5);
        assert_eq!(
            getter.container,
            KlibIrSignature::FileLocal { file: 7, slot: 1 }
        );
        let property = origins
            .get(&KlibIrSignature::FileLocal { file: 7, slot: 1 })
            .expect("the container's own origin is recorded when it is decoded");
        assert_eq!(property.id, 11);
        assert!(matches!(property.container, KlibIrSignature::Public(_)));
        assert_eq!(origins.len(), 2);
    }

    #[test]
    fn ordinary_ir_and_non_file_composites_remain_file_local() {
        let file = [0x3a, 0x00];
        let local = [0x20, 0x00];
        let public = public_signature();
        let file_composite = composite(0, 2);
        let local_composite = composite(1, 2);
        let entries = [
            file.as_slice(),
            local.as_slice(),
            public.as_slice(),
            file_composite.as_slice(),
            local_composite.as_slice(),
        ];
        let strings = ["fixture".to_owned(), "probe".to_owned()];

        let ordinary = SignatureTable::new(7, &entries, &strings)
            .symbol(3 << 8)
            .expect("the ordinary composite is valid");
        assert_eq!(
            ordinary.signature,
            KlibIrSignature::FileLocal { file: 7, slot: 3 }
        );

        let prepared_local = SignatureTable::prepared_inline(7, &entries, &strings)
            .symbol(4 << 8)
            .expect("the non-file prepared composite is valid");
        assert_eq!(
            prepared_local.signature,
            KlibIrSignature::FileLocal { file: 7, slot: 4 }
        );
    }

    #[test]
    fn prepared_declaration_table_identity_is_independent_of_the_private_copy_symbol() {
        let file = [0x3a, 0x00];
        let original = public_signature();
        let mut private_copy = public_signature();
        *private_copy
            .last_mut()
            .expect("public signature has a hash") ^= 1;
        let composite = composite(0, 2);
        let entries = [
            file.as_slice(),
            original.as_slice(),
            private_copy.as_slice(),
            composite.as_slice(),
        ];
        let strings = ["fixture".to_owned(), "probe".to_owned()];
        let mut table = SignatureTable::prepared_inline(0, &entries, &strings);

        let declaration = table
            .declaration_identity(1)
            .expect("the declaration table key is public");
        let copied = table
            .symbol(3 << 8)
            .expect("the private prepared copy has a valid symbol")
            .signature;

        assert_ne!(declaration, copied);
    }
}
