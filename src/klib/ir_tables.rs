//! The shared tables of one KLIB IR file: strings, identity signatures, types, bodies and file
//! entries, with the symbol, table and container encodings every IR component uses.
//!
//! Kotlin's serialized IR is a set of parallel tables addressed by index. Nothing here knows which
//! declarations or expressions a component holds; the component writers ([`super::inline_ir`]) own
//! that and call into these tables for identities and encodings.

use crate::metadata::id_signature::{KlibAccessorIdSignature, KlibPublicIdSignature};
use crate::metadata::protobuf::Pb;
use crate::types::{Ty, TyKind, TypeName};

pub(super) const FUNCTION_SYMBOL: u64 = 0;
pub(super) const CONSTRUCTOR_SYMBOL: u64 = 1;
pub(super) const FIELD_SYMBOL: u64 = 3;
pub(super) const VALUE_PARAMETER_SYMBOL: u64 = 4;
pub(super) const CLASS_SYMBOL: u64 = 6;
pub(super) const TYPE_PARAMETER_SYMBOL: u64 = 7;
pub(super) const ANONYMOUS_INITIALIZER_SYMBOL: u64 = 9;
pub(super) const RECEIVER_PARAMETER_SYMBOL: u64 = 11;
pub(super) const PROPERTY_SYMBOL: u64 = 12;

/// The tables of one IR file under construction.
#[derive(Default)]
pub(super) struct IrTables {
    pub(super) strings: Vec<String>,
    pub(super) signatures: Vec<Vec<u8>>,
    pub(super) types: Vec<Vec<u8>>,
    pub(super) bodies: Vec<Vec<u8>>,
    pub(super) file_entries: Vec<Vec<u8>>,
    /// Loops written so far. Kotlin IR identifies a loop, the target of its jumps, within its whole
    /// file.
    loops: u32,
    /// The type parameters a prepared inline copy erases, each with the type it writes instead.
    /// Empty outside such a copy.
    pub(super) erasures: Vec<(String, Ty)>,
}

impl IrTables {
    /// `parameters` pairs each type parameter in scope with its symbol.
    pub(super) fn ty(&mut self, ty: Ty, parameters: &[(Ty, u64)]) -> u32 {
        let erased = super::prepared_inline::erase(ty, &self.erasures);
        if erased != ty {
            return self.ty(erased, parameters);
        }
        let (inner, nullability) = match ty.kind() {
            TyKind::Nullable(inner) => (inner, 0),
            _ => (ty, 1),
        };
        // Kotlin IR writes a function type as its function class, marked
        // `@ExtensionFunctionType` when its first argument is the receiver.
        let extension = matches!(inner.kind(), TyKind::Fun(shape) if shape.has_receiver);
        let inner = super::body_tree::function_type_class(inner).unwrap_or(inner);
        let (classifier, arguments) = match inner.kind() {
            TyKind::Unit => (self.class_symbol(TypeName::from("kotlin/Unit")), &[][..]),
            TyKind::Nothing => (self.class_symbol(TypeName::from("kotlin/Nothing")), &[][..]),
            TyKind::BOOLEAN => (self.class_symbol(TypeName::from("kotlin/Boolean")), &[][..]),
            TyKind::Obj(classifier, arguments) => {
                (self.class_symbol(classifier), arguments.as_slice())
            }
            TyKind::TyParam(..) => {
                let (_, symbol) = parameters
                    .iter()
                    .find(|(parameter, _)| *parameter == inner)
                    .expect("supported type parameter is declared by the function");
                (*symbol, &[][..])
            }
            _ => unreachable!("unsupported types are filtered before serialization"),
        };
        let mut simple = Pb::new();
        if extension {
            let annotation = self.marker_annotation(&["kotlin"], "ExtensionFunctionType");
            simple.repeated_message(1, &annotation);
        }
        simple.field_varint(2, classifier);
        // `MARKED_NULLABLE` is protobuf zero, so omission does not mean non-null. Kotlin writes
        // `NOT_SPECIFIED` explicitly for an ordinary type-parameter occurrence.
        simple.field_varint(3, nullability);
        let arguments = arguments
            .iter()
            .map(|argument| {
                let (variance, ty) = match argument.kind() {
                    TyKind::InProjection(inner) => (2, inner),
                    TyKind::OutProjection(inner) => (3, inner),
                    TyKind::StarProjection(_) => return 0,
                    _ => (1, *argument),
                };
                (u64::from(self.ty(ty, parameters)) << 2) | variance
            })
            .collect::<Vec<_>>();
        simple.field_packed_varints(4, &arguments);
        let mut encoded = Pb::new();
        encoded.field_message(5, &simple);
        let bytes = encoded.canonical().into_bytes();
        if let Some(index) = self.types.iter().position(|existing| existing == &bytes) {
            return u32::try_from(index).expect("type table fits u32");
        }
        let index = u32::try_from(self.types.len()).expect("type table fits u32");
        self.types.push(bytes);
        index
    }

    /// A use of the annotation class `class` of `package` with no arguments, by its constructor.
    pub(super) fn marker_annotation(&mut self, package: &[&str], class: &str) -> Pb {
        use crate::metadata::id_signature::{
            callable_signature, CallableShape, ClassScope, DeclarationContainer, Placement,
        };
        use crate::metadata::semantic::KotlinType;

        let package = package
            .iter()
            .map(|segment| (*segment).to_owned())
            .collect::<Vec<_>>();
        let classes = [ClassScope::<KotlinType> {
            name: class,
            type_parameters: Vec::new(),
            expect: false,
        }];
        let signature = callable_signature(
            DeclarationContainer {
                package: &package,
                classes: &classes,
                native_interop_library: false,
            },
            &CallableShape::<KotlinType> {
                name: "<init>",
                contexts: Vec::new(),
                receiver: None,
                params: Vec::new(),
                vararg: None,
                type_parameters: Vec::new(),
                expect: false,
                placement: Placement::Ordinary,
            },
        )
        .expect("an annotation without arguments has an empty signable constructor");
        let constructor = symbol(self.public_signature(&signature), CONSTRUCTOR_SYMBOL);
        let mut annotation = Pb::new();
        annotation.field_varint(1, constructor);
        annotation.field_varint(2, 0);
        annotation
    }

    pub(super) fn class_symbol(&mut self, classifier: TypeName) -> u64 {
        let signature = KlibPublicIdSignature::classifier(classifier);
        let slot = self.public_signature(&signature);
        symbol(slot, CLASS_SYMBOL)
    }

    pub(super) fn public_signature(&mut self, signature: &KlibPublicIdSignature) -> u32 {
        let package = signature
            .package()
            .segments()
            .iter()
            .map(|segment| u64::from(self.string(segment)))
            .collect::<Vec<_>>();
        let declaration = signature
            .declaration()
            .segments()
            .iter()
            .map(|segment| u64::from(self.string(segment)))
            .collect::<Vec<_>>();
        let mut common = Pb::new();
        common.field_packed_varints(1, &package);
        common.field_packed_varints(2, &declaration);
        if let Some(member) = signature.member_id() {
            common.field_fixed64(6, member);
        }
        if signature.mask() != 0 {
            common.field_varint(4, signature.mask());
        }
        let mut encoded = Pb::new();
        encoded.field_message(1, &common);
        self.signature(encoded.canonical().into_bytes())
    }

    /// A property accessor's identity: its property's public signature, its own name, and its
    /// member hash.
    pub(super) fn accessor_signature(&mut self, signature: &KlibAccessorIdSignature) -> u32 {
        let property = self.public_signature(signature.property());
        let name = self.string(signature.name());
        let mut accessor = Pb::new();
        accessor.field_varint(1, u64::from(property));
        accessor.field_varint(2, u64::from(name));
        accessor.field_varint(3, signature.member_id());
        if signature.mask() != 0 {
            accessor.field_varint(4, signature.mask());
        }
        let mut encoded = Pb::new();
        encoded.field_message(3, &accessor);
        self.signature(encoded.canonical().into_bytes())
    }

    /// A loop id no other loop of the file has.
    pub(super) fn loop_id(&mut self) -> u32 {
        let id = self.loops;
        self.loops += 1;
        id
    }

    /// Append one body-table entry and answer its index.
    pub(super) fn body(&mut self, bytes: Vec<u8>) -> u32 {
        let index = u32::try_from(self.bodies.len()).expect("body table fits u32");
        self.bodies.push(bytes);
        index
    }

    pub(super) fn file_signature(&mut self) -> u32 {
        let mut encoded = Pb::new();
        encoded.field_message(7, &Pb::new());
        self.signature(encoded.into_bytes())
    }

    pub(super) fn composite_signature(&mut self, container: u32, inner: u32) -> u32 {
        let mut composite = Pb::new();
        composite.field_varint(1, u64::from(container));
        composite.field_varint(2, u64::from(inner));
        let mut encoded = Pb::new();
        encoded.field_message(5, &composite);
        self.signature(encoded.canonical().into_bytes())
    }

    /// The backing field of the property `property` signs: the property's identity scoped to the
    /// local `<BF>`.
    pub(super) fn field_signature(&mut self, property: &KlibPublicIdSignature) -> u32 {
        let property = self.public_signature(property);
        let name = self.string("<BF>");
        let mut local = Pb::new();
        local.field_packed_varints(1, &[u64::from(name)]);
        let mut encoded = Pb::new();
        encoded.field_message(6, &local);
        let local = self.signature(encoded.canonical().into_bytes());
        self.composite_signature(property, local)
    }

    pub(super) fn type_parameter_signature(&mut self, function: u32, index: usize) -> u32 {
        let name = self.string("<TP>");
        let mut local = Pb::new();
        local.field_packed_varints(1, &[u64::from(name)]);
        local.field_varint(
            2,
            u64::try_from(index).expect("type-parameter index fits u64"),
        );
        let mut encoded = Pb::new();
        encoded.field_message(6, &local);
        let local = self.signature(encoded.canonical().into_bytes());
        self.composite_signature(function, local)
    }

    pub(super) fn local_signature(&mut self) -> u32 {
        let mut encoded = Pb::new();
        encoded.field_varint(4, self.signatures.len() as u64);
        self.signature(encoded.into_bytes())
    }

    pub(super) fn signature(&mut self, bytes: Vec<u8>) -> u32 {
        if let Some(index) = self
            .signatures
            .iter()
            .position(|existing| existing == &bytes)
        {
            return u32::try_from(index).expect("signature table fits u32");
        }
        let index = u32::try_from(self.signatures.len()).expect("signature table fits u32");
        self.signatures.push(bytes);
        index
    }

    pub(super) fn string(&mut self, value: &str) -> u32 {
        if let Some(index) = self.strings.iter().position(|existing| existing == value) {
            return u32::try_from(index).expect("string table fits u32");
        }
        let index = u32::try_from(self.strings.len()).expect("string table fits u32");
        self.strings.push(value.to_owned());
        index
    }

    pub(super) fn file_entry(&mut self, name: &str) -> u32 {
        let name = self.string(name);
        let mut entry = Pb::new();
        entry.field_varint(4, u64::from(name));
        let bytes = entry.into_bytes();
        if let Some(index) = self
            .file_entries
            .iter()
            .position(|existing| existing == &bytes)
        {
            return u32::try_from(index).expect("file-entry table fits u32");
        }
        let index = u32::try_from(self.file_entries.len()).expect("file-entry table fits u32");
        self.file_entries.push(bytes);
        index
    }
}

pub(super) fn symbol(signature: u32, kind: u64) -> u64 {
    (u64::from(signature) << 8) | kind
}

/// Kotlin IR's `BinaryLattice`: even bits carry the name, odd bits the type index.
pub(super) fn lattice(name: u32, ty: u32) -> u64 {
    fn spread(mut value: u64) -> u64 {
        value &= 0xffff_ffff;
        value = (value | (value << 16)) & 0x0000_ffff_0000_ffff;
        value = (value | (value << 8)) & 0x00ff_00ff_00ff_00ff;
        value = (value | (value << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
        value = (value | (value << 2)) & 0x3333_3333_3333_3333;
        (value | (value << 1)) & 0x5555_5555_5555_5555
    }
    spread(u64::from(name)) | (spread(u64::from(ty)) << 1)
}

pub(super) fn entry_table(entries: &[Vec<u8>]) -> Vec<u8> {
    let count = i32::try_from(entries.len()).expect("entry count fits i32");
    let mut bytes = (-count).to_be_bytes().to_vec();
    for entry in entries {
        leb128(
            u64::try_from(entry.len()).expect("entry length fits u64"),
            &mut bytes,
        );
    }
    for entry in entries {
        bytes.extend_from_slice(entry);
    }
    bytes
}

pub(super) fn declaration_table(declarations: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        &u32::try_from(declarations.len())
            .expect("declaration count fits u32")
            .to_be_bytes(),
    );
    let mut offset = 4 + declarations.len() * 12;
    for (signature, declaration) in declarations {
        bytes.extend_from_slice(&signature.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(offset)
                .expect("declaration offset fits u32")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(
            &u32::try_from(declaration.len())
                .expect("declaration length fits u32")
                .to_be_bytes(),
        );
        offset += declaration.len();
    }
    for (_, declaration) in declarations {
        bytes.extend_from_slice(declaration);
    }
    bytes
}

pub(super) fn per_file(file: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + file.len());
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(
        &u32::try_from(file.len())
            .expect("per-file data fits u32")
            .to_be_bytes(),
    );
    bytes.extend(file);
    bytes
}

pub(super) fn leb128(mut value: u64, bytes: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        bytes.push(byte | if value == 0 { 0 } else { 0x80 });
        if value == 0 {
            return;
        }
    }
}
