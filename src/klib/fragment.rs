//! Encoding a KLIB `PackageFragment` — the `.knm` entries a klib's `linkdata` holds.
//!
//! The message is the shared Kotlin metadata `PackageFragment` plus the extension fields a KLIB adds
//! to it, and the layout below is read off artifacts the reference compiler wrote rather than
//! inferred from a schema:
//!
//! ```text
//! 1   StringTable           always emitted, empty message included
//! 2   QualifiedNameTable    always emitted, empty message included
//! 3   Package               always emitted, empty message included
//! 4   Class                 repeated; one fragment per class is how kotlinc splits a package
//! 171 (varint)              present on the fragment carrying the Package, absent on a class one
//! 172 is_empty              1 when the fragment declares nothing
//! 173 fq_name               the package's name; empty for the root package
//! 174 class_name            packed string-table indices of the classes in this fragment
//! ```
//!
//! Fields 1, 2 and 3 are written even with nothing in them: the root package's fragment is twelve
//! bytes, `1`/`2`/`3` each an empty message, and a writer that omitted them would be shorter than
//! what every consumer of a klib has been handed.
//!
//! This module encodes the FRAMING and takes each sub-message as bytes, the same division the
//! container writer and the metadata reader keep: what a `Class` or a `Package` message contains is
//! the metadata encoder's to say.

pub mod declarations;

/// One `PackageFragment` under construction.
#[derive(Default)]
pub struct FragmentBuilder {
    string_table: Vec<u8>,
    qualified_name_table: Vec<u8>,
    package: Vec<u8>,
    classes: Vec<Vec<u8>>,
    class_name_ids: Vec<u32>,
    fq_name: String,
}

impl FragmentBuilder {
    /// A fragment for `fq_name`; the empty name is the root package.
    pub fn new(fq_name: &str) -> Self {
        Self {
            fq_name: fq_name.to_string(),
            ..Self::default()
        }
    }

    /// The encoded `StringTable` body (its `string` entries are field 1, repeated).
    pub fn string_table(&mut self, body: Vec<u8>) -> &mut Self {
        self.string_table = body;
        self
    }

    /// The encoded `QualifiedNameTable` body.
    pub fn qualified_name_table(&mut self, body: Vec<u8>) -> &mut Self {
        self.qualified_name_table = body;
        self
    }

    /// The encoded `Package` body — which already carries its own extension field 171, written
    /// inside the message by [`declarations::encode_package`].
    pub fn package(&mut self, body: Vec<u8>) -> &mut Self {
        self.package = body;
        self
    }

    /// Add one encoded `Class` body and the string-table index of its name.
    pub fn class(&mut self, body: Vec<u8>, name_id: u32) -> &mut Self {
        self.classes.push(body);
        self.class_name_ids.push(name_id);
        self
    }

    /// Whether this fragment declares nothing — the root package's usual state.
    fn is_empty(&self) -> bool {
        self.string_table.is_empty()
            && self.qualified_name_table.is_empty()
            && self.package.is_empty()
            && self.classes.is_empty()
    }

    /// The fragment's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        push_message(&mut out, 1, &self.string_table);
        push_message(&mut out, 2, &self.qualified_name_table);
        push_message(&mut out, 3, &self.package);
        for class in &self.classes {
            push_message(&mut out, 4, class);
        }
        push_varint_field(&mut out, 172, u64::from(self.is_empty()));
        push_message(&mut out, 173, self.fq_name.as_bytes());
        if !self.class_name_ids.is_empty() {
            let mut packed = Vec::new();
            for id in &self.class_name_ids {
                push_varint(&mut packed, u64::from(*id));
            }
            push_message(&mut out, 174, &packed);
        }
        out
    }
}

/// The fragment a package with no declarations of its own gets.
///
/// Every klib the reference distribution ships has one for the root package, and a resolver walking
/// a qualifier needs it: the package exists even when nothing is declared in it.
pub fn empty_fragment(fq_name: &str) -> Vec<u8> {
    FragmentBuilder::new(fq_name).encode()
}

pub(crate) fn push_message(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    push_varint(out, u64::from(field) << 3 | 2);
    push_varint(out, body.len() as u64);
    out.extend_from_slice(body);
}

pub(crate) fn push_varint_field(out: &mut Vec<u8>, field: u32, value: u64) {
    push_varint(out, u64::from(field) << 3);
    push_varint(out, value);
}

pub(crate) fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root package's fragment from a reference metadata klib, byte for byte. Twelve bytes: the
    /// three tables each present and empty, `is_empty = 1`, and an empty `fq_name`.
    #[test]
    fn the_root_packages_empty_fragment_matches_the_reference() {
        assert_eq!(
            empty_fragment(""),
            b"\n\x00\x12\x00\x1a\x00\xe0\n\x01\xea\n\x00".to_vec()
        );
    }

    /// A named empty package differs from the root one only in `fq_name`.
    #[test]
    fn a_named_empty_package_carries_its_name() {
        assert_eq!(
            empty_fragment("lib"),
            b"\n\x00\x12\x00\x1a\x00\xe0\n\x01\xea\n\x03lib".to_vec()
        );
    }

    /// A fragment carrying one class writes the tables, the class, `is_empty = 0`, the name, and the
    /// packed index of the class's name — and no field 171, which belongs to the package fragment.
    #[test]
    fn a_class_fragment_packs_its_class_name_index() {
        let mut builder = FragmentBuilder::new("lib");
        builder
            .string_table(b"\n\x03lib".to_vec())
            .qualified_name_table(b"\n\x02\x10\x00".to_vec())
            .class(b"\x10\x01".to_vec(), 1);
        assert_eq!(
            builder.encode(),
            [
                b"\n\x05\n\x03lib".as_slice(),
                b"\x12\x04\n\x02\x10\x00".as_slice(),
                b"\x1a\x00".as_slice(),
                b"\x22\x02\x10\x01".as_slice(),
                b"\xe0\n\x00".as_slice(),
                b"\xea\n\x03lib".as_slice(),
                b"\xf2\n\x01\x01".as_slice(),
            ]
            .concat()
        );
    }

    /// A fragment carrying a package writes no packed class names: 174 is for the class fragments.
    #[test]
    fn a_package_fragment_names_no_classes() {
        let mut builder = FragmentBuilder::new("lib");
        builder
            .string_table(b"\n\x03lib".to_vec())
            .package(b"\x1a\x02\x10\x00".to_vec());
        let encoded = builder.encode();
        assert!(
            encoded.ends_with(b"\xe0\n\x00\xea\n\x03lib"),
            "is_empty, then the name: {encoded:02x?}"
        );
        assert!(
            !encoded.windows(2).any(|pair| pair == b"\xf2\n"),
            "and no packed class names"
        );
    }
}

/// A fragment's own `StringTable`: plain strings, interned in the order they are first needed.
///
/// A KLIB fragment carries its names inline, which is the whole difference from the JVM `@Metadata`
/// carrier: there the strings live in the annotation's `d2` array with a `StringTableTypes` record
/// per entry remapping it, and a class is named by an encoded string. Here a string is just a string
/// and a class is named through the [`QualifiedNameTable`] beside it.
#[derive(Default)]
pub struct StringTable {
    strings: Vec<String>,
    index: std::collections::HashMap<String, u32>,
}

impl StringTable {
    /// The index of `string`, interning it on first use.
    pub fn intern(&mut self, string: &str) -> u32 {
        if let Some(index) = self.index.get(string) {
            return *index;
        }
        let index = self.strings.len() as u32;
        self.strings.push(string.to_string());
        self.index.insert(string.to_string(), index);
        index
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// The encoded `StringTable` body (`string` is field 1, repeated).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for string in &self.strings {
            push_message(&mut out, 1, string.as_bytes());
        }
        out
    }
}

/// What a [`QualifiedName`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QualifiedNameKind {
    Class,
    Package,
}

/// One `QualifiedNameTable.QualifiedName`: a parent entry, a short name, and which of the two it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QualifiedName {
    /// Index of the enclosing entry in the same table; `None` for a top-level name.
    pub parent: Option<u32>,
    /// Index of the short name in the fragment's [`StringTable`].
    pub short_name: u32,
    pub kind: QualifiedNameKind,
}

/// A fragment's `QualifiedNameTable`: how a fragment names a classifier or a package.
///
/// `PACKAGE` is the protobuf default for `kind`, so a package entry omits the field and a class
/// entry writes it as `0` — which is why a reference table's package rows are four bytes and its
/// class rows eight.
#[derive(Default)]
pub struct QualifiedNameTable {
    names: Vec<QualifiedName>,
    index: std::collections::HashMap<QualifiedName, u32>,
}

impl QualifiedNameTable {
    /// The index of `name`, interning it on first use.
    pub fn intern(&mut self, name: QualifiedName) -> u32 {
        if let Some(index) = self.index.get(&name) {
            return *index;
        }
        let index = self.names.len() as u32;
        self.names.push(name);
        self.index.insert(name, index);
        index
    }

    /// Intern a dotted package name, one entry per segment, parented through the table.
    pub fn intern_package(&mut self, strings: &mut StringTable, fqname: &str) -> Option<u32> {
        if fqname.is_empty() {
            return None;
        }
        let mut parent = None;
        for segment in fqname.split('.') {
            let short_name = strings.intern(segment);
            parent = Some(self.intern(QualifiedName {
                parent,
                short_name,
                kind: QualifiedNameKind::Package,
            }));
        }
        parent
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The encoded `QualifiedNameTable` body (`qualified_name` is field 1, repeated).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for name in &self.names {
            let mut row = Vec::new();
            if let Some(parent) = name.parent {
                push_varint_field(&mut row, 1, u64::from(parent));
            }
            push_varint_field(&mut row, 2, u64::from(name.short_name));
            if name.kind == QualifiedNameKind::Class {
                push_varint_field(&mut row, 3, 0);
            }
            push_message(&mut out, 1, &row);
        }
        out
    }
}

#[cfg(test)]
mod name_table_tests {
    use super::*;

    /// The string table a reference fragment carries for `package lib` declaring `class Box<T>(val
    /// value: T)` with a `describe(prefix: String)` member, byte for byte: plain length-delimited
    /// strings, in first-use order, with no per-entry record.
    #[test]
    fn a_string_table_is_plain_strings_in_first_use_order() {
        let mut strings = StringTable::default();
        for string in [
            "lib", "Box", "T", "kotlin", "Any", "value", "Lib.kt", "describe", "String", "prefix",
            "get",
        ] {
            strings.intern(string);
        }
        assert_eq!(strings.intern("Box"), 1, "a repeat returns the first index");
        assert_eq!(
            strings.encode(),
            b"\n\x03lib\n\x03Box\n\x01T\n\x06kotlin\n\x03Any\n\x05value\n\x06Lib.kt\
              \n\x08describe\n\x06String\n\x06prefix\n\x03get"
                .to_vec()
        );
    }

    /// The qualified-name table from the same reference fragment, byte for byte. A package row omits
    /// `kind` (PACKAGE is the default) and a class row writes it as `0`.
    #[test]
    fn a_qualified_name_table_matches_the_reference() {
        let mut strings = StringTable::default();
        for string in [
            "lib", "Box", "T", "kotlin", "Any", "value", "Lib.kt", "describe", "String", "prefix",
            "get",
        ] {
            strings.intern(string);
        }
        let mut names = QualifiedNameTable::default();
        let lib = names
            .intern_package(&mut strings, "lib")
            .expect("a named package");
        names.intern(QualifiedName {
            parent: Some(lib),
            short_name: strings.intern("Box"),
            kind: QualifiedNameKind::Class,
        });
        let kotlin = names
            .intern_package(&mut strings, "kotlin")
            .expect("a named package");
        names.intern(QualifiedName {
            parent: Some(kotlin),
            short_name: strings.intern("Any"),
            kind: QualifiedNameKind::Class,
        });
        names.intern(QualifiedName {
            parent: Some(kotlin),
            short_name: strings.intern("String"),
            kind: QualifiedNameKind::Class,
        });
        assert_eq!(
            names.encode(),
            [
                b"\n\x02\x10\x00".as_slice(),                 // lib, a package
                b"\n\x06\x08\x00\x10\x01\x18\x00".as_slice(), // lib.Box, a class
                b"\n\x02\x10\x03".as_slice(),                 // kotlin, a package
                b"\n\x06\x08\x02\x10\x04\x18\x00".as_slice(), // kotlin.Any
                b"\n\x06\x08\x02\x10\x08\x18\x00".as_slice(), // kotlin.String
            ]
            .concat()
        );
    }

    /// A dotted package is one entry per segment, each parented on the one before — so `a.b` costs
    /// two rows and `b` alone is not a name in the table.
    #[test]
    fn a_dotted_package_is_interned_segment_by_segment() {
        let mut strings = StringTable::default();
        let mut names = QualifiedNameTable::default();
        let leaf = names
            .intern_package(&mut strings, "a.b")
            .expect("a named package");
        assert_eq!(leaf, 1, "the leaf is the second row");
        assert_eq!(
            names.encode(),
            b"\n\x02\x10\x00\n\x04\x08\x00\x10\x01".to_vec()
        );
        assert_eq!(
            names.intern_package(&mut strings, "a.b"),
            Some(leaf),
            "interning the same package again adds no rows"
        );
        assert_eq!(
            names.intern_package(&mut strings, ""),
            None,
            "the root package has no entry"
        );
    }
}
