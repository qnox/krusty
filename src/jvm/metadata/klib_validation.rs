//! Checked KLIB metadata decoding.
//!
//! Distribution KLIBs are dependencies, not compiler-owned resources. Their header and every
//! declaration fragment cross one fallible boundary before any semantic declaration is published.

use super::{parse_qname, BuiltinPackage, QName};

mod semantic;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageFragmentDecodeError {
    pub offset: usize,
    pub detail: String,
}

impl std::fmt::Display for PackageFragmentDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} at byte {}", self.detail, self.offset)
    }
}

impl std::error::Error for PackageFragmentDecodeError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KlibModuleHeader {
    pub module_name: String,
    pub package_fragment_names: Vec<String>,
}

pub(super) struct DecodedPackageFragment<'a> {
    pub strings: Vec<String>,
    pub qnames: Vec<QName>,
    pub package: Option<&'a [u8]>,
    pub classes: Vec<&'a [u8]>,
    pub file_annotations: Vec<&'a [u8]>,
    pub class_names: Vec<u64>,
    pub inventory: SemanticInventory,
}

#[derive(Default)]
pub(super) struct SemanticInventory {
    package_functions: usize,
    classes: std::collections::BTreeMap<String, ClassInventory>,
}

#[derive(Default)]
struct ClassInventory {
    supertypes: usize,
    type_parameters: usize,
    constructors: usize,
    members: usize,
}

#[derive(Clone, Copy)]
enum MessageKind {
    Package,
    Class,
    Constructor,
    Function,
    Property,
    ValueParameter,
    TypeAlias,
    TypeParameter,
    Type,
    TypeArgument,
    TypeTable,
    QualifiedName,
    Annotation,
    AnnotationArgument,
    AnnotationValue,
    EnumEntry,
    VersionRequirementTable,
    VersionRequirement,
    Contract,
    Effect,
    Expression,
    CompilerPluginData,
}

impl MessageKind {
    fn name(self) -> &'static str {
        match self {
            Self::Package => "package declaration",
            Self::Class => "class declaration",
            Self::Constructor => "constructor declaration",
            Self::Function => "function declaration",
            Self::Property => "property declaration",
            Self::ValueParameter => "value-parameter declaration",
            Self::TypeAlias => "type-alias declaration",
            Self::TypeParameter => "type-parameter declaration",
            Self::Type => "type",
            Self::TypeArgument => "type argument",
            Self::TypeTable => "type table",
            Self::QualifiedName => "qualified-name entry",
            Self::Annotation => "annotation",
            Self::AnnotationArgument => "annotation argument",
            Self::AnnotationValue => "annotation value",
            Self::EnumEntry => "enum entry",
            Self::VersionRequirementTable => "version-requirement table",
            Self::VersionRequirement => "version requirement",
            Self::Contract => "contract",
            Self::Effect => "contract effect",
            Self::Expression => "contract expression",
            Self::CompilerPluginData => "compiler-plugin data",
        }
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
    base: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            offset: 0,
            base,
        }
    }

    fn absolute(&self) -> usize {
        self.base + self.offset
    }

    fn error(&self, detail: impl Into<String>) -> PackageFragmentDecodeError {
        PackageFragmentDecodeError {
            offset: self.absolute(),
            detail: detail.into(),
        }
    }

    fn varint(&mut self, context: &str) -> Result<u64, PackageFragmentDecodeError> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| self.error(format!("truncated {context}")))?;
            self.offset += 1;
            if shift == 63 && byte > 1 {
                return Err(self.error(format!("oversized {context}")));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(self.error(format!("oversized {context}")))
    }

    fn bytes(
        &mut self,
        length: u64,
        context: &str,
    ) -> Result<(&'a [u8], usize), PackageFragmentDecodeError> {
        let length =
            usize::try_from(length).map_err(|_| self.error(format!("oversized {context}")))?;
        let start = self.offset;
        let end = start
            .checked_add(length)
            .ok_or_else(|| self.error(format!("oversized {context}")))?;
        let bytes = self
            .bytes
            .get(start..end)
            .ok_or_else(|| self.error(format!("truncated {context}")))?;
        self.offset = end;
        Ok((bytes, self.base + start))
    }

    fn length_delimited(
        &mut self,
        context: &str,
    ) -> Result<(&'a [u8], usize), PackageFragmentDecodeError> {
        let length = self.varint(&format!("{context} length"))?;
        self.bytes(length, context)
    }

    fn skip(&mut self, wire: u64, context: &str) -> Result<(), PackageFragmentDecodeError> {
        match wire {
            0 => {
                self.varint(context)?;
            }
            1 => {
                self.bytes(8, context)?;
            }
            2 => {
                self.length_delimited(context)?;
            }
            5 => {
                self.bytes(4, context)?;
            }
            _ => return Err(self.error(format!("invalid wire type {wire} in {context}"))),
        }
        Ok(())
    }
}

fn field(cursor: &mut Cursor<'_>, context: &str) -> Result<(u64, u64), PackageFragmentDecodeError> {
    let tag = cursor.varint(&format!("{context} field tag"))?;
    let number = tag >> 3;
    if number == 0 {
        return Err(cursor.error(format!("zero field number in {context}")));
    }
    Ok((number, tag & 7))
}

fn require_wire(
    cursor: &Cursor<'_>,
    actual: u64,
    expected: u64,
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    if actual == expected {
        Ok(())
    } else {
        Err(cursor.error(format!(
            "field in {context} has wire type {actual}, expected {expected}"
        )))
    }
}

fn nested(kind: MessageKind, field: u64) -> Option<MessageKind> {
    use MessageKind as K;
    match (kind, field) {
        (K::Package, 3) => Some(K::Function),
        (K::Package, 4) => Some(K::Property),
        (K::Package, 5) => Some(K::TypeAlias),
        (K::Package, 30) => Some(K::TypeTable),
        (K::Package, 32) => Some(K::VersionRequirementTable),
        (K::Class, 5) => Some(K::TypeParameter),
        (K::Class, 6 | 18 | 20) => Some(K::Type),
        (K::Class, 8) => Some(K::Constructor),
        (K::Class, 9) => Some(K::Function),
        (K::Class, 10) => Some(K::Property),
        (K::Class, 11) => Some(K::TypeAlias),
        (K::Class, 13) => Some(K::EnumEntry),
        (K::Class, 25 | 170) => Some(K::Annotation),
        (K::Class, 30) => Some(K::TypeTable),
        (K::Class, 32) => Some(K::VersionRequirementTable),
        (K::Class, 33) => Some(K::CompilerPluginData),
        (K::Constructor, 2) => Some(K::ValueParameter),
        (K::Constructor, 3 | 170) => Some(K::Annotation),
        (K::Constructor, 32) => Some(K::CompilerPluginData),
        (K::Function, 3 | 5 | 10 | 14) => Some(K::Type),
        (K::Function, 4) => Some(K::TypeParameter),
        (K::Function, 6 | 13) => Some(K::ValueParameter),
        (K::Function, 12 | 34 | 170 | 171) => Some(K::Annotation),
        (K::Function, 30) => Some(K::TypeTable),
        (K::Function, 32) => Some(K::Contract),
        (K::Function, 33) => Some(K::CompilerPluginData),
        (K::Property, 3 | 5 | 12 | 18) => Some(K::Type),
        (K::Property, 4) => Some(K::TypeParameter),
        (K::Property, 6 | 17) => Some(K::ValueParameter),
        (K::Property, 14 | 15 | 16 | 33 | 34 | 35 | 170 | 177 | 178 | 181 | 182 | 183) => {
            Some(K::Annotation)
        }
        (K::Property, 40 | 41) => Some(K::Contract),
        (K::Property, 173) => Some(K::AnnotationValue),
        (K::Property, 32) => Some(K::CompilerPluginData),
        (K::ValueParameter, 3 | 4 | 9) => Some(K::Type),
        (K::ValueParameter, 7 | 170) => Some(K::Annotation),
        (K::ValueParameter, 8) => Some(K::AnnotationValue),
        (K::TypeAlias, 3) => Some(K::TypeParameter),
        (K::TypeAlias, 4 | 6) => Some(K::Type),
        (K::TypeAlias, 8) => Some(K::Annotation),
        (K::TypeAlias, 32) => Some(K::CompilerPluginData),
        (K::TypeParameter, 5) => Some(K::Type),
        (K::TypeParameter, 100 | 170) => Some(K::Annotation),
        (K::Type, 2) => Some(K::TypeArgument),
        (K::Type, 5 | 10 | 13) => Some(K::Type),
        (K::Type, 100 | 170) => Some(K::Annotation),
        (K::TypeArgument, 2) => Some(K::Type),
        (K::TypeTable, 1) => Some(K::Type),
        (K::Annotation, 2) => Some(K::AnnotationArgument),
        (K::AnnotationArgument, 2) => Some(K::AnnotationValue),
        (K::AnnotationValue, 8) => Some(K::Annotation),
        (K::AnnotationValue, 9) => Some(K::AnnotationValue),
        (K::EnumEntry, 2 | 170) => Some(K::Annotation),
        (K::VersionRequirementTable, 1) => Some(K::VersionRequirement),
        (K::Contract, 1) => Some(K::Effect),
        (K::Effect, 2 | 3) => Some(K::Expression),
        (K::Expression, 4) => Some(K::Type),
        (K::Expression, 6 | 7) => Some(K::Expression),
        _ => None,
    }
}

fn known_varint(kind: MessageKind, field: u64) -> bool {
    use MessageKind as K;
    match kind {
        K::Class => matches!(field, 1..=4 | 7 | 16..=17 | 19 | 21 | 31),
        K::Constructor => matches!(field, 1 | 31),
        K::Function => matches!(field, 1..=2 | 7..=9 | 11 | 15 | 31 | 172),
        K::Property => matches!(field, 1..=2 | 7..=11 | 13 | 19 | 31 | 176),
        K::ValueParameter => matches!(field, 1..=2 | 5..=6 | 10),
        K::TypeAlias => matches!(field, 1..=2 | 5 | 7 | 31 | 170),
        K::TypeParameter => matches!(field, 1..=4 | 6),
        K::Type => matches!(field, 1 | 3..=4 | 6..=9 | 11..=12 | 14),
        K::TypeArgument => matches!(field, 1 | 3),
        K::TypeTable => field == 2,
        K::QualifiedName => matches!(field, 1..=3),
        K::Annotation => field == 1,
        K::AnnotationArgument => field == 1,
        K::AnnotationValue => matches!(field, 1..=2 | 5..=7 | 10..=11),
        K::EnumEntry => matches!(field, 1 | 171),
        K::VersionRequirement => matches!(field, 1..=6),
        K::Effect => matches!(field, 1 | 4..=5),
        K::Expression => matches!(field, 1..=3 | 5),
        K::Package => field == 171,
        K::CompilerPluginData => field == 1,
        K::VersionRequirementTable | K::Contract => false,
    }
}

fn packed_varint(kind: MessageKind, field: u64) -> bool {
    use MessageKind as K;
    matches!(
        (kind, field),
        (K::Class, 2 | 7 | 16 | 21 | 31)
            | (K::Constructor, 31)
            | (K::Function, 11 | 31)
            | (K::Property, 13 | 31)
            | (K::TypeAlias, 31)
            | (K::TypeParameter, 6)
    )
}

fn validate_packed_varints(
    bytes: &[u8],
    base: usize,
    context: &str,
) -> Result<(), PackageFragmentDecodeError> {
    let mut packed = Cursor::new(bytes, base);
    while packed.offset < bytes.len() {
        packed.varint(context)?;
    }
    Ok(())
}

fn required_fields(kind: MessageKind) -> &'static [u64] {
    use MessageKind as K;
    match kind {
        K::Class => &[3],
        K::Function | K::Property | K::ValueParameter | K::TypeAlias => &[2],
        K::TypeParameter => &[1, 2],
        K::QualifiedName => &[2],
        K::Annotation => &[1],
        K::AnnotationArgument => &[1, 2],
        K::AnnotationValue => &[1],
        K::CompilerPluginData => &[1, 2],
        _ => &[],
    }
}

fn required_alternatives(kind: MessageKind) -> &'static [&'static [u64]] {
    use MessageKind as K;
    match kind {
        K::Function => &[&[3, 7]],
        K::Property => &[&[3, 9]],
        K::ValueParameter => &[&[3, 5]],
        K::TypeAlias => &[&[4, 5], &[6, 7]],
        _ => &[],
    }
}

fn validate_message(
    bytes: &[u8],
    base: usize,
    kind: MessageKind,
) -> Result<(), PackageFragmentDecodeError> {
    let context = kind.name();
    let mut cursor = Cursor::new(bytes, base);
    let mut seen = std::collections::BTreeSet::new();
    while cursor.offset < bytes.len() {
        let (number, wire) = field(&mut cursor, context)?;
        seen.insert(number);
        if let Some(child_kind) = nested(kind, number) {
            require_wire(&cursor, wire, 2, context)?;
            let (child, child_base) = cursor.length_delimited(child_kind.name())?;
            validate_message(child, child_base, child_kind)?;
        } else if known_varint(kind, number) {
            if packed_varint(kind, number) && wire == 2 {
                let (packed, packed_base) = cursor.length_delimited(context)?;
                validate_packed_varints(packed, packed_base, context)?;
            } else {
                require_wire(&cursor, wire, 0, context)?;
                cursor.varint(context)?;
            }
        } else if matches!(
            (kind, number),
            (MessageKind::Class, 176)
                | (MessageKind::Constructor, 173)
                | (MessageKind::Function, 174)
                | (MessageKind::Property, 180)
        ) {
            require_wire(&cursor, wire, 2, context)?;
            let (text, text_base) = cursor.length_delimited(context)?;
            std::str::from_utf8(text).map_err(|error| PackageFragmentDecodeError {
                offset: text_base + error.valid_up_to(),
                detail: format!("invalid UTF-8 in {context}"),
            })?;
        } else if matches!((kind, number), (MessageKind::AnnotationValue, 3)) {
            require_wire(&cursor, wire, 5, context)?;
            cursor.skip(wire, context)?;
        } else if matches!((kind, number), (MessageKind::AnnotationValue, 4)) {
            require_wire(&cursor, wire, 1, context)?;
            cursor.skip(wire, context)?;
        } else if matches!((kind, number), (MessageKind::CompilerPluginData, 2)) {
            require_wire(&cursor, wire, 2, context)?;
            cursor.length_delimited(context)?;
        } else {
            cursor.skip(wire, context)?;
        }
    }
    for required in required_fields(kind) {
        if !seen.contains(required) {
            return Err(PackageFragmentDecodeError {
                offset: base,
                detail: format!("{context} is missing required field {required}"),
            });
        }
    }
    for alternatives in required_alternatives(kind) {
        if !alternatives.iter().any(|field| seen.contains(field)) {
            return Err(PackageFragmentDecodeError {
                offset: base,
                detail: format!("{context} is missing one of required fields {alternatives:?}"),
            });
        }
    }
    Ok(())
}

fn count_fields(body: &[u8], fields: &[u64]) -> Result<usize, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut count = 0;
    while cursor.offset < body.len() {
        let (number, wire) = field(&mut cursor, "validated declaration")?;
        if fields.contains(&number) {
            count += 1;
        }
        cursor.skip(wire, "validated declaration")?;
    }
    Ok(count)
}

fn class_inventory(
    body: &[u8],
    strings: &[String],
    qnames: &[QName],
) -> Result<(String, ClassInventory), PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(body, 0);
    let mut fq_name = None;
    let mut inventory = ClassInventory::default();
    while cursor.offset < body.len() {
        let (number, wire) = field(&mut cursor, "class declaration")?;
        match (number, wire) {
            (2, 0) => {
                cursor.varint("class supertype id")?;
                inventory.supertypes += 1;
            }
            (2, 2) => {
                let (packed, base) = cursor.length_delimited("class supertype ids")?;
                let mut packed = Cursor::new(packed, base);
                while packed.offset < packed.bytes.len() {
                    packed.varint("class supertype id")?;
                    inventory.supertypes += 1;
                }
            }
            (3, 0) => fq_name = Some(cursor.varint("class qualified-name id")?),
            (5, 2) => {
                cursor.length_delimited("class type parameter")?;
                inventory.type_parameters += 1;
            }
            (6, 2) => {
                cursor.length_delimited("class supertype")?;
                inventory.supertypes += 1;
            }
            (8, 2) => {
                cursor.length_delimited("class constructor")?;
                inventory.constructors += 1;
            }
            (9 | 10, 2) => {
                cursor.length_delimited("class member")?;
                inventory.members += 1;
            }
            (_, wire) => cursor.skip(wire, "class declaration")?,
        }
    }
    let fq_name = fq_name.ok_or_else(|| PackageFragmentDecodeError {
        offset: 0,
        detail: "class declaration is missing required field 3".to_string(),
    })?;
    let fq_name_index = usize::try_from(fq_name).map_err(|_| PackageFragmentDecodeError {
        offset: 0,
        detail: format!("class qualified name {fq_name} exceeds the host index range"),
    })?;
    let fq_name = qnames
        .get(fq_name_index)
        .map(|_| super::resolve_qname(qnames, strings, fq_name_index as i64))
        .filter(|name| !name.is_empty())
        .ok_or_else(|| PackageFragmentDecodeError {
            offset: 0,
            detail: format!("class references absent qualified name {fq_name}"),
        })?;
    Ok((fq_name, inventory))
}

fn semantic_inventory(
    package: Option<&[u8]>,
    classes: &[&[u8]],
    strings: &[String],
    qnames: &[QName],
) -> Result<SemanticInventory, PackageFragmentDecodeError> {
    let mut inventory = SemanticInventory {
        package_functions: package.map_or(Ok(0), |body| count_fields(body, &[3]))?,
        classes: std::collections::BTreeMap::new(),
    };
    for body in classes {
        let (name, class) = class_inventory(body, strings, qnames)?;
        if inventory.classes.insert(name.clone(), class).is_some() {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!("duplicate class identity {name}"),
            });
        }
    }
    Ok(inventory)
}

pub(super) fn verify_semantic_inventory(
    expected: &SemanticInventory,
    package: &BuiltinPackage,
) -> Result<(), PackageFragmentDecodeError> {
    if package.functions.len() != expected.package_functions {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: format!(
                "package declared {} functions but {} decoded completely",
                expected.package_functions,
                package.functions.len()
            ),
        });
    }
    if package.classes.len() != expected.classes.len() {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: format!(
                "package fragment declared {} classes but {} decoded completely",
                expected.classes.len(),
                package.classes.len()
            ),
        });
    }
    for (name, expected) in &expected.classes {
        let actual = package
            .classes
            .get(name)
            .ok_or_else(|| PackageFragmentDecodeError {
                offset: 0,
                detail: format!("class {name} did not decode completely"),
            })?;
        if actual.supertype_tys.len() != expected.supertypes
            || actual.type_params.len() != expected.type_parameters
            || actual.constructors.len() != expected.constructors
            || actual.members.len() != expected.members
        {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!(
                    "class {name} contains a declaration that did not decode completely"
                ),
            });
        }
    }
    Ok(())
}

fn decode_string_table(
    bytes: &[u8],
    base: usize,
) -> Result<Vec<String>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(bytes, base);
    let mut strings = Vec::new();
    while cursor.offset < bytes.len() {
        let (number, wire) = field(&mut cursor, "string table")?;
        if number == 1 {
            require_wire(&cursor, wire, 2, "string table")?;
            let (string, string_base) = cursor.length_delimited("string-table entry")?;
            let string =
                std::str::from_utf8(string).map_err(|error| PackageFragmentDecodeError {
                    offset: string_base + error.valid_up_to(),
                    detail: "invalid UTF-8 in string-table entry".to_string(),
                })?;
            strings.push(string.to_string());
        } else {
            cursor.skip(wire, "string table")?;
        }
    }
    Ok(strings)
}

fn decode_qualified_names(
    bytes: &[u8],
    base: usize,
) -> Result<Vec<QName>, PackageFragmentDecodeError> {
    let mut cursor = Cursor::new(bytes, base);
    let mut qnames = Vec::new();
    while cursor.offset < bytes.len() {
        let (number, wire) = field(&mut cursor, "qualified-name table")?;
        if number == 1 {
            require_wire(&cursor, wire, 2, "qualified-name table")?;
            let (entry, entry_base) = cursor.length_delimited("qualified-name entry")?;
            validate_message(entry, entry_base, MessageKind::QualifiedName)?;
            qnames.push(parse_qname(entry));
        } else {
            cursor.skip(wire, "qualified-name table")?;
        }
    }
    Ok(qnames)
}

pub(super) fn decode_package_fragment(
    bytes: &[u8],
) -> Result<DecodedPackageFragment<'_>, PackageFragmentDecodeError> {
    if bytes.is_empty() {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: "empty package fragment".to_string(),
        });
    }
    let mut cursor = Cursor::new(bytes, 0);
    let mut strings = None;
    let mut qnames = None;
    let mut package = None;
    let mut classes = Vec::new();
    let mut file_annotations = Vec::new();
    let mut class_names = Vec::new();
    while cursor.offset < bytes.len() {
        let (number, wire) = field(&mut cursor, "package fragment")?;
        match number {
            1 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (body, base) = cursor.length_delimited("package-fragment string table")?;
                if strings.is_some() {
                    return Err(cursor.error("duplicate package-fragment string table"));
                }
                strings = Some(decode_string_table(body, base)?);
            }
            2 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (body, base) =
                    cursor.length_delimited("package-fragment qualified-name table")?;
                if qnames.is_some() {
                    return Err(cursor.error("duplicate package-fragment qualified-name table"));
                }
                qnames = Some(decode_qualified_names(body, base)?);
            }
            3 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (body, base) = cursor.length_delimited("package declaration")?;
                if package.is_some() {
                    return Err(cursor.error("duplicate package declaration"));
                }
                validate_message(body, base, MessageKind::Package)?;
                package = Some(body);
            }
            4 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (body, base) = cursor.length_delimited("class declaration")?;
                validate_message(body, base, MessageKind::Class)?;
                classes.push(body);
            }
            5 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (body, base) = cursor.length_delimited("file annotation")?;
                validate_message(body, base, MessageKind::Annotation)?;
                file_annotations.push(body);
            }
            172 => {
                require_wire(&cursor, wire, 0, "package fragment")?;
                cursor.varint("package-fragment empty marker")?;
            }
            173 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (name, base) = cursor.length_delimited("package-fragment fq name")?;
                std::str::from_utf8(name).map_err(|error| PackageFragmentDecodeError {
                    offset: base + error.valid_up_to(),
                    detail: "invalid UTF-8 in package-fragment fq name".to_string(),
                })?;
            }
            174 => {
                require_wire(&cursor, wire, 2, "package fragment")?;
                let (packed, base) = cursor.length_delimited("package-fragment class names")?;
                validate_packed_varints(packed, base, "package-fragment class name")?;
                let mut packed = Cursor::new(packed, base);
                while packed.offset < packed.bytes.len() {
                    class_names.push(packed.varint("package-fragment class name")?);
                }
            }
            _ => cursor.skip(wire, "package fragment")?,
        }
    }
    let strings = strings.unwrap_or_default();
    let qnames = qnames.unwrap_or_default();
    for (index, qname) in qnames.iter().enumerate() {
        if qname.short >= strings.len() {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!(
                    "qualified-name entry {index} references absent string {}",
                    qname.short
                ),
            });
        }
        if qname.parent < -1 || qname.parent >= index as i64 {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!(
                    "qualified-name entry {index} has invalid parent {}",
                    qname.parent
                ),
            });
        }
        if qname.kind > 2 {
            return Err(PackageFragmentDecodeError {
                offset: 0,
                detail: format!(
                    "qualified-name entry {index} has invalid kind {}",
                    qname.kind
                ),
            });
        }
    }
    let inventory = semantic_inventory(package, &classes, &strings, &qnames)?;
    Ok(DecodedPackageFragment {
        strings,
        qnames,
        package,
        classes,
        file_annotations,
        class_names,
        inventory,
    })
}

pub fn parse_module_header(bytes: &[u8]) -> Result<KlibModuleHeader, PackageFragmentDecodeError> {
    if bytes.is_empty() {
        return Err(PackageFragmentDecodeError {
            offset: 0,
            detail: "empty KLIB module header".to_string(),
        });
    }
    let mut cursor = Cursor::new(bytes, 0);
    let mut module_name = None;
    let mut packages = Vec::new();
    let mut package_set = std::collections::BTreeSet::new();
    while cursor.offset < bytes.len() {
        let (number, wire) = field(&mut cursor, "KLIB module header")?;
        match number {
            1 | 7 | 8 => {
                require_wire(&cursor, wire, 2, "KLIB module header")?;
                let (text, base) = cursor.length_delimited("KLIB module-header string")?;
                let text =
                    std::str::from_utf8(text).map_err(|error| PackageFragmentDecodeError {
                        offset: base + error.valid_up_to(),
                        detail: "invalid UTF-8 in KLIB module header".to_string(),
                    })?;
                if number == 1 {
                    if module_name.replace(text.to_string()).is_some() {
                        return Err(cursor.error("duplicate KLIB module name"));
                    }
                } else if number == 7 {
                    if !package_set.insert(text.to_string()) {
                        return Err(
                            cursor.error(format!("duplicate KLIB package-fragment name {text:?}"))
                        );
                    }
                    packages.push(text.to_string());
                }
            }
            2 => {
                require_wire(&cursor, wire, 0, "KLIB module header")?;
                cursor.varint("KLIB module flags")?;
            }
            _ => cursor.skip(wire, "KLIB module header")?,
        }
    }
    let module_name = module_name.ok_or_else(|| PackageFragmentDecodeError {
        offset: 0,
        detail: "KLIB module header is missing required module name".to_string(),
    })?;
    Ok(KlibModuleHeader {
        module_name,
        package_fragment_names: packages,
    })
}

/// Decode one dependency-owned fragment atomically. Structural and semantic parsing are both
/// fallible: a nested declaration is published only after all of its names, types, parameters and
/// table references have resolved successfully.
pub fn parse_package_fragment_checked(
    bytes: &[u8],
) -> Result<BuiltinPackage, PackageFragmentDecodeError> {
    semantic::parse(decode_package_fragment(bytes)?)
}

pub(super) fn strip_builtins_header(data: &[u8]) -> Option<&[u8]> {
    let count = u32::from_be_bytes(*data.get(0..4)?.first_chunk::<4>()?) as usize;
    data.get(4 + 4 * count..)
}
