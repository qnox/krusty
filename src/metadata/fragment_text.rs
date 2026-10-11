//! A KLIB `PackageFragment` as text, for tests that compare two libraries' declarations.
//!
//! Every field of every declaration message is printed, in field order, with string and
//! qualified-name indices resolved and type-table references replaced by the type they name.
//! Two fragments that print the same declare the same things with the same flags, whatever order
//! their writers interned strings and types in. A field this schema does not name is printed by
//! number, so nothing a writer adds goes unseen.

use std::fmt::Write as _;

use super::decode::{packed_varints, Pb};

/// One field as it is on the wire.
#[derive(Clone, Copy)]
enum Wire<'a> {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(&'a [u8]),
}

fn fields(bytes: &[u8]) -> Vec<(u64, Wire<'_>)> {
    let mut cursor = Pb::new(bytes);
    let mut out = Vec::new();
    while !cursor.at_end() {
        let tag = cursor.varint().expect("a field tag");
        let value = match tag & 7 {
            0 => Wire::Varint(cursor.varint().expect("a varint")),
            1 => Wire::Fixed64(u64::from_le_bytes(
                cursor
                    .bytes(8)
                    .expect("8 bytes")
                    .try_into()
                    .expect("8 bytes"),
            )),
            2 => {
                let length = cursor.varint().expect("a length") as usize;
                Wire::Bytes(cursor.bytes(length).expect("the field's bytes"))
            }
            5 => Wire::Fixed32(u32::from_le_bytes(
                cursor
                    .bytes(4)
                    .expect("4 bytes")
                    .try_into()
                    .expect("4 bytes"),
            )),
            wire => panic!("wire type {wire}"),
        };
        out.push((tag >> 3, value));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Message {
    Package,
    Class,
    Constructor,
    Function,
    Property,
    ValueParameter,
    TypeAlias,
    EnumEntry,
    TypeParameter,
    Annotation,
}

/// How a field's value is printed.
#[derive(Clone, Copy)]
enum Kind {
    Int,
    Str,
    QName,
    Type,
    /// A type-table index; printed as the type, under the name of the inline field.
    TypeRef,
    PackedStr,
    PackedQName,
    PackedTypeRef,
    Nested(Message),
    ConstValue,
    /// Read for its contents elsewhere, not printed.
    Skip,
}

/// The field's name, how it is printed, and the field number it sorts as (a type-table
/// reference sorts with the inline field it replaces).
fn schema(message: Message, number: u64) -> Option<(&'static str, Kind, u64)> {
    use Kind::*;
    let entry = |name, kind| Some((name, kind, number));
    let as_field = |name, kind, sort| Some((name, kind, sort));
    match (message, number) {
        (Message::Package, 3) => entry("function", Nested(Message::Function)),
        (Message::Package, 4) => entry("property", Nested(Message::Property)),
        (Message::Package, 5) => entry("typealias", Nested(Message::TypeAlias)),
        (Message::Package, 30) => entry("type_table", Skip),
        (Message::Package, 171) => entry("package_fq_name", QName),

        (Message::Class, 1) => entry("flags", Int),
        (Message::Class, 3) => entry("fq_name", QName),
        (Message::Class, 4) => entry("companion_object_name", Str),
        (Message::Class, 5) => entry("type_parameter", Nested(Message::TypeParameter)),
        (Message::Class, 6) => entry("supertype", Type),
        (Message::Class, 2) => as_field("supertype", PackedTypeRef, 6),
        (Message::Class, 7) => entry("nested_class_name", PackedStr),
        (Message::Class, 8) => entry("constructor", Nested(Message::Constructor)),
        (Message::Class, 9) => entry("function", Nested(Message::Function)),
        (Message::Class, 10) => entry("property", Nested(Message::Property)),
        (Message::Class, 11) => entry("typealias", Nested(Message::TypeAlias)),
        (Message::Class, 13) => entry("enum_entry", Nested(Message::EnumEntry)),
        (Message::Class, 16) => entry("sealed_subclass", PackedQName),
        (Message::Class, 17) => entry("inline_class_underlying_property_name", Str),
        (Message::Class, 18) => entry("inline_class_underlying_type", Type),
        (Message::Class, 19) => as_field("inline_class_underlying_type", TypeRef, 18),
        (Message::Class, 30) => entry("type_table", Skip),
        (Message::Class, 170) => entry("annotation", Nested(Message::Annotation)),
        (Message::Class, 175) => entry("class_file", Int),

        (Message::Constructor, 1) => entry("flags", Int),
        (Message::Constructor, 2) => entry("value_parameter", Nested(Message::ValueParameter)),
        (Message::Constructor, 170) => entry("annotation", Nested(Message::Annotation)),

        (Message::Function, 9) => entry("flags", Int),
        (Message::Function, 2) => entry("name", Str),
        (Message::Function, 3) => entry("return_type", Type),
        (Message::Function, 7) => as_field("return_type", TypeRef, 3),
        (Message::Function, 4) => entry("type_parameter", Nested(Message::TypeParameter)),
        (Message::Function, 5) => entry("receiver_type", Type),
        (Message::Function, 8) => as_field("receiver_type", TypeRef, 5),
        (Message::Function, 6) => entry("value_parameter", Nested(Message::ValueParameter)),
        (Message::Function, 170) => entry("annotation", Nested(Message::Annotation)),
        (Message::Function, 172) => entry("function_file", Int),

        (Message::Property, 11) => entry("flags", Int),
        (Message::Property, 2) => entry("name", Str),
        (Message::Property, 3) => entry("return_type", Type),
        (Message::Property, 9) => as_field("return_type", TypeRef, 3),
        (Message::Property, 4) => entry("type_parameter", Nested(Message::TypeParameter)),
        (Message::Property, 5) => entry("receiver_type", Type),
        (Message::Property, 10) => as_field("receiver_type", TypeRef, 5),
        (Message::Property, 6) => entry("setter_value_parameter", Nested(Message::ValueParameter)),
        (Message::Property, 7) => entry("getter_flags", Int),
        (Message::Property, 8) => entry("setter_flags", Int),
        (Message::Property, 170) => entry("annotation", Nested(Message::Annotation)),
        (Message::Property, 173) => entry("compile_time_value", ConstValue),
        (Message::Property, 176) => entry("property_file", Int),
        (Message::Property, 177) => entry("getter_annotation", Nested(Message::Annotation)),
        (Message::Property, 178) => entry("setter_annotation", Nested(Message::Annotation)),

        (Message::ValueParameter, 1) => entry("flags", Int),
        (Message::ValueParameter, 2) => entry("name", Str),
        (Message::ValueParameter, 3) => entry("type", Type),
        (Message::ValueParameter, 5) => as_field("type", TypeRef, 3),
        (Message::ValueParameter, 4) => entry("vararg_element_type", Type),
        (Message::ValueParameter, 6) => as_field("vararg_element_type", TypeRef, 4),
        (Message::ValueParameter, 170) => entry("annotation", Nested(Message::Annotation)),

        (Message::TypeAlias, 1) => entry("flags", Int),
        (Message::TypeAlias, 2) => entry("name", Str),
        (Message::TypeAlias, 3) => entry("type_parameter", Nested(Message::TypeParameter)),
        (Message::TypeAlias, 4) => entry("underlying_type", Type),
        (Message::TypeAlias, 5) => as_field("underlying_type", TypeRef, 4),
        (Message::TypeAlias, 6) => entry("expanded_type", Type),
        (Message::TypeAlias, 7) => as_field("expanded_type", TypeRef, 6),
        (Message::TypeAlias, 8) => entry("annotation", Nested(Message::Annotation)),

        (Message::EnumEntry, 1) => entry("name", Str),
        (Message::EnumEntry, 170) => entry("annotation", Nested(Message::Annotation)),
        (Message::EnumEntry, 171) => entry("ordinal", Int),

        (Message::TypeParameter, 1) => entry("id", Int),
        (Message::TypeParameter, 2) => entry("name", Str),
        (Message::TypeParameter, 3) => entry("reified", Int),
        (Message::TypeParameter, 4) => entry("variance", Int),
        (Message::TypeParameter, 5) => entry("upper_bound", Type),
        (Message::TypeParameter, 6) => as_field("upper_bound", TypeRef, 5),
        _ => None,
    }
}

/// The type table in scope: its types and the index from which they are nullable.
#[derive(Clone, Copy)]
struct TypeTable<'a> {
    types: &'a [&'a [u8]],
    first_nullable: Option<usize>,
}

struct Printer<'a> {
    strings: Vec<String>,
    /// `(parent, short name, kind)`; kind 0 is a class, 1 a package, 2 local.
    qualified: Vec<(i64, u64, u64)>,
    out: String,
    tables: Vec<(Vec<&'a [u8]>, Option<usize>)>,
}

/// The fragment `bytes` as text.
pub(crate) fn render_fragment(bytes: &[u8]) -> String {
    let top = fields(bytes);
    let mut printer = Printer {
        strings: Vec::new(),
        qualified: Vec::new(),
        out: String::new(),
        tables: Vec::new(),
    };
    for (number, value) in &top {
        match (number, value) {
            (1, Wire::Bytes(table)) => {
                for (_, string) in fields(table) {
                    let Wire::Bytes(string) = string else {
                        panic!("a string")
                    };
                    printer
                        .strings
                        .push(String::from_utf8(string.to_vec()).expect("UTF-8"));
                }
            }
            (2, Wire::Bytes(table)) => {
                for (_, name) in fields(table) {
                    let Wire::Bytes(name) = name else {
                        panic!("a qualified name")
                    };
                    let mut entry = (-1_i64, 0, 1);
                    for (field, value) in fields(name) {
                        let Wire::Varint(value) = value else {
                            panic!("a varint")
                        };
                        match field {
                            1 => entry.0 = value as i32 as i64,
                            2 => entry.1 = value,
                            3 => entry.2 = value,
                            _ => panic!("qualified name field {field}"),
                        }
                    }
                    printer.qualified.push(entry);
                }
            }
            _ => {}
        }
    }
    for (number, value) in &top {
        match (number, value) {
            (1 | 2, _) => {}
            (3, Wire::Bytes(package)) => printer.message("package", Message::Package, package, 0),
            (4, Wire::Bytes(class)) => printer.message("class", Message::Class, class, 0),
            (172, Wire::Varint(value)) => {
                let _ = writeln!(printer.out, "is_empty: {value}");
            }
            (173, Wire::Bytes(name)) => {
                let _ = writeln!(printer.out, "fq_name: {}", String::from_utf8_lossy(name));
            }
            (174, Wire::Bytes(names)) => {
                let names = packed_varints(names).expect("packed names");
                let names = names
                    .iter()
                    .map(|&name| printer.qname(name as i64))
                    .collect::<Vec<_>>();
                let _ = writeln!(printer.out, "class_name: {}", names.join(", "));
            }
            (number, value) => {
                let raw = printer.raw(*value);
                let _ = writeln!(printer.out, "#{number}: {raw}");
            }
        }
    }
    printer.out
}

impl<'a> Printer<'a> {
    fn string(&self, index: u64) -> String {
        self.strings
            .get(index as usize)
            .cloned()
            .unwrap_or_else(|| format!("<string {index}>"))
    }

    fn qname(&self, index: i64) -> String {
        if index < 0 {
            return "<root>".to_string();
        }
        let Some(&(parent, short, kind)) = self.qualified.get(index as usize) else {
            return format!("<qname {index}>");
        };
        let short = self.string(short);
        if parent < 0 {
            return short;
        }
        let parent_kind = self.qualified[parent as usize].2;
        let separator = if parent_kind == 0 && kind == 0 {
            "."
        } else {
            "/"
        };
        format!("{}{separator}{short}", self.qname(parent))
    }

    fn raw(&self, value: Wire<'_>) -> String {
        match value {
            Wire::Varint(value) => value.to_string(),
            Wire::Fixed32(value) => format!("fixed32 {value:#x}"),
            Wire::Fixed64(value) => format!("fixed64 {value:#x}"),
            Wire::Bytes(bytes) => format!("bytes {bytes:?}"),
        }
    }

    fn table(&self) -> TypeTable<'_> {
        match self.tables.last() {
            Some((types, first_nullable)) => TypeTable {
                types,
                first_nullable: *first_nullable,
            },
            None => TypeTable {
                types: &[],
                first_nullable: None,
            },
        }
    }

    fn line(&mut self, depth: usize, text: &str) {
        let _ = writeln!(self.out, "{}{text}", "  ".repeat(depth));
    }

    /// A declaration message: its own type table is in scope for its fields.
    fn message(&mut self, name: &str, kind: Message, bytes: &'a [u8], depth: usize) {
        let mut own = fields(bytes);
        let table = own
            .iter()
            .find_map(|(number, value)| match (number, value) {
                (30, Wire::Bytes(table))
                    if matches!(kind, Message::Package | Message::Class | Message::Function) =>
                {
                    Some(*table)
                }
                _ => None,
            });
        if let Some(table) = table {
            let mut types = Vec::new();
            let mut first_nullable = None;
            for (number, value) in fields(table) {
                match (number, value) {
                    (1, Wire::Bytes(ty)) => types.push(ty),
                    (2, Wire::Varint(index)) => first_nullable = Some(index as usize),
                    _ => panic!("type table field {number}"),
                }
            }
            self.tables.push((types, first_nullable));
        }
        self.line(depth, &format!("{name} {{"));
        own.sort_by_key(|(number, _)| schema(kind, *number).map_or(*number, |entry| entry.2));
        for (number, value) in own {
            let Some((field, field_kind, _)) = schema(kind, number) else {
                let raw = self.raw(value);
                self.line(depth + 1, &format!("#{number}: {raw}"));
                continue;
            };
            match (field_kind, value) {
                (Kind::Skip, _) => {}
                (Kind::Int, Wire::Varint(value)) => {
                    self.line(depth + 1, &format!("{field}: {value}"));
                }
                (Kind::Str, Wire::Varint(value)) => {
                    let string = self.string(value);
                    self.line(depth + 1, &format!("{field}: {string}"));
                }
                (Kind::QName, Wire::Varint(value)) => {
                    let name = self.qname(value as i32 as i64);
                    self.line(depth + 1, &format!("{field}: {name}"));
                }
                (Kind::Type, Wire::Bytes(ty)) => {
                    let ty = self.ty(ty, false);
                    self.line(depth + 1, &format!("{field}: {ty}"));
                }
                (Kind::TypeRef, Wire::Varint(index)) => {
                    let ty = self.type_ref(index);
                    self.line(depth + 1, &format!("{field}: {ty}"));
                }
                (Kind::PackedTypeRef, Wire::Bytes(indices)) => {
                    for index in packed_varints(indices).expect("packed type ids") {
                        let ty = self.type_ref(index);
                        self.line(depth + 1, &format!("{field}: {ty}"));
                    }
                }
                (Kind::PackedStr, Wire::Bytes(indices)) => {
                    let names = packed_varints(indices)
                        .expect("packed names")
                        .into_iter()
                        .map(|index| self.string(index))
                        .collect::<Vec<_>>();
                    self.line(depth + 1, &format!("{field}: {}", names.join(", ")));
                }
                (Kind::PackedQName, Wire::Bytes(indices)) => {
                    let names = packed_varints(indices)
                        .expect("packed names")
                        .into_iter()
                        .map(|index| self.qname(index as i64))
                        .collect::<Vec<_>>();
                    self.line(depth + 1, &format!("{field}: {}", names.join(", ")));
                }
                (Kind::Nested(Message::Annotation), Wire::Bytes(annotation)) => {
                    let annotation = self.annotation(annotation);
                    self.line(depth + 1, &format!("{field}: {annotation}"));
                }
                (Kind::Nested(inner), Wire::Bytes(bytes)) => {
                    self.message(field, inner, bytes, depth + 1);
                }
                (Kind::ConstValue, Wire::Bytes(value)) => {
                    let value = self.value(value);
                    self.line(depth + 1, &format!("{field}: {value}"));
                }
                (_, value) => {
                    let raw = self.raw(value);
                    self.line(depth + 1, &format!("{field}?: {raw}"));
                }
            }
        }
        self.line(depth, "}");
        if table.is_some() {
            self.tables.pop();
        }
    }

    fn type_ref(&self, index: u64) -> String {
        let table = self.table();
        let Some(ty) = table.types.get(index as usize) else {
            return format!("<type {index}>");
        };
        let nullable = table
            .first_nullable
            .is_some_and(|first| index as usize >= first);
        self.ty(ty, nullable)
    }

    /// A `Type` as Kotlin-like text: `@A Class<Arguments>?`, then `/* = ^Alias */` for its
    /// abbreviation and `{#n=…}` for anything else it carries.
    fn ty(&self, bytes: &[u8], table_nullable: bool) -> String {
        let mut head = String::new();
        let mut arguments = Vec::new();
        let mut nullable = table_nullable;
        let mut extra = Vec::new();
        let mut abbreviation = None;
        let mut annotations = Vec::new();
        for (number, value) in fields(bytes) {
            match (number, value) {
                (1, Wire::Varint(flags)) => extra.push(format!("flags={flags}")),
                (2, Wire::Bytes(argument)) => arguments.push(self.argument(argument)),
                (3, Wire::Varint(value)) => nullable = value != 0,
                (6, Wire::Varint(name)) => head = self.qname(name as i64),
                (7, Wire::Varint(id)) => head = format!("T#{id}"),
                (9, Wire::Varint(name)) => head = format!("T:{}", self.string(name)),
                (12, Wire::Varint(name)) => head = format!("^{}", self.qname(name as i64)),
                (13, Wire::Bytes(abbreviated)) => abbreviation = Some(self.ty(abbreviated, false)),
                (14, Wire::Varint(index)) => abbreviation = Some(self.type_ref(index)),
                (170, Wire::Bytes(annotation)) => annotations.push(self.annotation(annotation)),
                (number, value) => extra.push(format!("#{number}={}", self.raw(value))),
            }
        }
        let mut out = String::new();
        for annotation in annotations {
            out.push_str(&annotation);
            out.push(' ');
        }
        out.push_str(&head);
        if !arguments.is_empty() {
            let _ = write!(out, "<{}>", arguments.join(", "));
        }
        if nullable {
            out.push('?');
        }
        if let Some(abbreviation) = abbreviation {
            let _ = write!(out, " /* = {abbreviation} */");
        }
        if !extra.is_empty() {
            let _ = write!(out, " {{{}}}", extra.join(", "));
        }
        out
    }

    fn argument(&self, bytes: &[u8]) -> String {
        let mut projection = 2;
        let mut ty = None;
        for (number, value) in fields(bytes) {
            match (number, value) {
                (1, Wire::Varint(value)) => projection = value,
                (2, Wire::Bytes(argument)) => ty = Some(self.ty(argument, false)),
                (3, Wire::Varint(index)) => ty = Some(self.type_ref(index)),
                (number, value) => panic!("type argument field {number}: {}", self.raw(value)),
            }
        }
        let ty = ty.unwrap_or_default();
        match projection {
            0 => format!("in {ty}"),
            1 => format!("out {ty}"),
            2 => ty,
            3 => "*".to_string(),
            other => format!("<projection {other}> {ty}"),
        }
    }

    fn annotation(&self, bytes: &[u8]) -> String {
        let mut class = String::new();
        let mut arguments = Vec::new();
        for (number, value) in fields(bytes) {
            match (number, value) {
                (1, Wire::Varint(id)) => class = self.qname(id as i64),
                (2, Wire::Bytes(argument)) => {
                    let mut name = String::new();
                    let mut argument_value = String::new();
                    for (number, value) in fields(argument) {
                        match (number, value) {
                            (1, Wire::Varint(id)) => name = self.string(id),
                            (2, Wire::Bytes(value)) => argument_value = self.value(value),
                            (number, value) => {
                                panic!("annotation argument field {number}: {}", self.raw(value))
                            }
                        }
                    }
                    arguments.push(format!("{name} = {argument_value}"));
                }
                (number, value) => panic!("annotation field {number}: {}", self.raw(value)),
            }
        }
        format!("@{class}({})", arguments.join(", "))
    }

    /// An `Annotation.Argument.Value`.
    fn value(&self, bytes: &[u8]) -> String {
        let mut kind = None;
        let mut parts = Vec::new();
        for (number, value) in fields(bytes) {
            match (number, value) {
                (1, Wire::Varint(value)) => kind = Some(value),
                (2, Wire::Varint(value)) => {
                    let decoded = ((value >> 1) as i64) ^ -((value & 1) as i64);
                    parts.push(decoded.to_string());
                }
                (3, Wire::Fixed32(bits)) => parts.push(format!("{}f", f32::from_bits(bits))),
                (4, Wire::Fixed64(bits)) => parts.push(format!("{}", f64::from_bits(bits))),
                (5, Wire::Varint(id)) => parts.push(format!("{:?}", self.string(id))),
                (6, Wire::Varint(id)) => parts.push(self.qname(id as i64)),
                (7, Wire::Varint(id)) => parts.push(format!(".{}", self.string(id))),
                (8, Wire::Bytes(annotation)) => parts.push(self.annotation(annotation)),
                (9, Wire::Bytes(element)) => parts.push(self.value(element)),
                (10, Wire::Varint(flags)) => parts.push(format!("flags={flags}")),
                (11, Wire::Varint(dimensions)) => parts.push(format!("dims={dimensions}")),
                (number, value) => parts.push(format!("#{number}={}", self.raw(value))),
            }
        }
        let kind = match kind {
            None => "byte",
            Some(0) => "byte",
            Some(1) => "char",
            Some(2) => "short",
            Some(3) => "int",
            Some(4) => "long",
            Some(5) => "float",
            Some(6) => "double",
            Some(7) => "boolean",
            Some(8) => "string",
            Some(9) => "class",
            Some(10) => "enum",
            Some(11) => "annotation",
            Some(12) => "array",
            Some(_) => "?",
        };
        format!("{kind}({})", parts.join(" "))
    }
}
