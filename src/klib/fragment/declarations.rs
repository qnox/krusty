//! Encoding the DECLARATIONS a `PackageFragment` carries: `Type`, `Function`, `Property`,
//! `Constructor`, `Class` and `Package`.
//!
//! The shared Kotlin metadata schema, written for the KLIB carrier. Every field number below is read
//! off fragments the reference compiler wrote, and each is named for what its value turned out to be
//! rather than assumed from a JVM-side analogue:
//!
//! ```text
//! Type            argument = 2, nullable = 3, class_name = 6, type_parameter = 7,
//!                 type_parameter_name = 9
//! Argument        projection = 1, type_id = 3
//! TypeParameter   id = 1, name = 2, reified = 3, variance = 4, upper_bound_id = 6
//! ValueParameter  flags = 1, name = 2, type_id = 5
//! Function        name = 2, type_parameter = 4, value_parameter = 6, return_type_id = 7,
//!                 receiver_type_id = 8, flags = 9, file = 172
//! Property        flags = 1, name = 2, return_type_id = 9, file = 176
//! Constructor     flags = 1, value_parameter = 2
//! Class           supertype_id = 2 (packed), fq_name = 3, type_parameter = 5, constructor = 8,
//!                 function = 9, property = 10, type_table = 30, file = 175
//! Package         function = 3, property = 4, type_table = 30
//! ```
//!
//! Types travel through the fragment's TYPE TABLE rather than inline: a declaration holds an id and
//! the table holds the `Type`, which is why `return_type_id` appears and `return_type` does not. The
//! table is interned in first-use order, so the id a declaration carries depends on the order the
//! declarations are encoded in — the writer therefore builds the table as it goes rather than
//! collecting types up front.

use super::{push_message, push_varint, push_varint_field, StringTable};

/// A type, as a fragment records it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FragmentType {
    /// A classifier, named through the qualified-name table, with its type arguments.
    Class {
        qualified_name: u32,
        arguments: Vec<TypeArgument>,
        nullable: bool,
    },
    /// A type parameter of the ENCLOSING CLASS, referenced by the id that class assigned it.
    ClassTypeParameter { id: u32, nullable: bool },
    /// A type parameter of the declaration itself, referenced by its name in the string table.
    ///
    /// A class member names an enclosing class's parameter by id while a top-level function names
    /// its own by name; both spellings occur in one distribution and mean different things.
    NamedTypeParameter { name: u32, nullable: bool },
}

/// One type argument: a projection and the type-table id of the argument's own type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeArgument {
    /// `Projection`: 0 IN, 1 OUT, 2 INV, 3 STAR. Omitted at the protobuf default.
    pub projection: Option<u32>,
    pub type_id: u32,
}

/// The fragment's type table, interning each distinct `Type` in first-use order.
#[derive(Default)]
pub struct TypeTable {
    types: Vec<FragmentType>,
    index: std::collections::HashMap<FragmentType, u32>,
}

impl TypeTable {
    /// The id of `ty`, interning it on first use.
    pub fn intern(&mut self, ty: FragmentType) -> u32 {
        if let Some(id) = self.index.get(&ty) {
            return *id;
        }
        let id = self.types.len() as u32;
        self.types.push(ty.clone());
        self.index.insert(ty, id);
        id
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// The encoded `TypeTable` body (`type` is field 1, repeated).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for ty in &self.types {
            let mut row = Vec::new();
            match ty {
                FragmentType::Class {
                    qualified_name,
                    arguments,
                    nullable,
                } => {
                    for argument in arguments {
                        let mut body = Vec::new();
                        if let Some(projection) = argument.projection {
                            push_varint_field(&mut body, 1, u64::from(projection));
                        }
                        push_varint_field(&mut body, 3, u64::from(argument.type_id));
                        push_message(&mut row, 2, &body);
                    }
                    if *nullable {
                        push_varint_field(&mut row, 3, 1);
                    }
                    push_varint_field(&mut row, 6, u64::from(*qualified_name));
                }
                FragmentType::ClassTypeParameter { id, nullable } => {
                    if *nullable {
                        push_varint_field(&mut row, 3, 1);
                    }
                    push_varint_field(&mut row, 7, u64::from(*id));
                }
                FragmentType::NamedTypeParameter { name, nullable } => {
                    if *nullable {
                        push_varint_field(&mut row, 3, 1);
                    }
                    push_varint_field(&mut row, 9, u64::from(*name));
                }
            }
            push_message(&mut out, 1, &row);
        }
        out
    }
}

/// One declared type parameter.
pub struct TypeParameterMeta {
    pub id: u32,
    /// String-table index of the parameter's source name.
    pub name: u32,
    pub reified: bool,
    /// `Variance`: 0 IN, 1 OUT, 2 INV. Omitted at the default.
    pub variance: Option<u32>,
    pub upper_bound_ids: Vec<u32>,
}

/// One value parameter.
pub struct ValueParameterMeta {
    /// The parameter's flag word; omitted at its protobuf default.
    pub flags: Option<u64>,
    /// String-table index of the parameter's name.
    pub name: u32,
    pub type_id: u32,
}

/// One function, member or top-level.
pub struct FunctionMeta {
    /// The function's flag word; omitted at its protobuf default.
    pub flags: Option<u64>,
    /// String-table index of the function's name.
    pub name: u32,
    pub type_parameters: Vec<TypeParameterMeta>,
    pub value_parameters: Vec<ValueParameterMeta>,
    pub return_type_id: u32,
    pub receiver_type_id: Option<u32>,
    /// String-table index of the declaring file's name.
    pub file: Option<u32>,
}

/// One property.
pub struct PropertyMeta {
    pub flags: Option<u64>,
    pub name: u32,
    pub return_type_id: u32,
    pub file: Option<u32>,
}

/// One constructor.
pub struct ConstructorMeta {
    pub flags: Option<u64>,
    pub value_parameters: Vec<ValueParameterMeta>,
}

/// One class.
pub struct ClassMeta {
    pub flags: Option<u64>,
    /// Qualified-name-table index of the class's own name.
    pub fq_name: u32,
    /// Type-table ids of the direct supertypes.
    pub supertype_ids: Vec<u32>,
    pub type_parameters: Vec<TypeParameterMeta>,
    pub constructors: Vec<ConstructorMeta>,
    pub functions: Vec<FunctionMeta>,
    pub properties: Vec<PropertyMeta>,
    pub file: Option<u32>,
}

fn encode_type_parameter(parameter: &TypeParameterMeta) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint_field(&mut out, 1, u64::from(parameter.id));
    push_varint_field(&mut out, 2, u64::from(parameter.name));
    if parameter.reified {
        push_varint_field(&mut out, 3, 1);
    }
    if let Some(variance) = parameter.variance {
        push_varint_field(&mut out, 4, u64::from(variance));
    }
    for bound in &parameter.upper_bound_ids {
        push_varint_field(&mut out, 6, u64::from(*bound));
    }
    out
}

fn encode_value_parameter(parameter: &ValueParameterMeta) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(flags) = parameter.flags {
        push_varint_field(&mut out, 1, flags);
    }
    push_varint_field(&mut out, 2, u64::from(parameter.name));
    push_varint_field(&mut out, 5, u64::from(parameter.type_id));
    out
}

/// One `Function` message body.
pub fn encode_function(function: &FunctionMeta) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint_field(&mut out, 2, u64::from(function.name));
    for parameter in &function.type_parameters {
        push_message(&mut out, 4, &encode_type_parameter(parameter));
    }
    for parameter in &function.value_parameters {
        push_message(&mut out, 6, &encode_value_parameter(parameter));
    }
    push_varint_field(&mut out, 7, u64::from(function.return_type_id));
    if let Some(receiver) = function.receiver_type_id {
        push_varint_field(&mut out, 8, u64::from(receiver));
    }
    if let Some(flags) = function.flags {
        push_varint_field(&mut out, 9, flags);
    }
    if let Some(file) = function.file {
        push_varint_field(&mut out, 172, u64::from(file));
    }
    out
}

/// One `Property` message body.
pub fn encode_property(property: &PropertyMeta) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(flags) = property.flags {
        push_varint_field(&mut out, 1, flags);
    }
    push_varint_field(&mut out, 2, u64::from(property.name));
    push_varint_field(&mut out, 9, u64::from(property.return_type_id));
    if let Some(file) = property.file {
        push_varint_field(&mut out, 176, u64::from(file));
    }
    out
}

/// One `Class` message body, without its type table.
pub fn encode_class(class: &ClassMeta) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(flags) = class.flags {
        push_varint_field(&mut out, 1, flags);
    }
    if !class.supertype_ids.is_empty() {
        let mut packed = Vec::new();
        for id in &class.supertype_ids {
            push_varint(&mut packed, u64::from(*id));
        }
        push_message(&mut out, 2, &packed);
    }
    push_varint_field(&mut out, 3, u64::from(class.fq_name));
    for parameter in &class.type_parameters {
        push_message(&mut out, 5, &encode_type_parameter(parameter));
    }
    for constructor in &class.constructors {
        let mut body = Vec::new();
        if let Some(flags) = constructor.flags {
            push_varint_field(&mut body, 1, flags);
        }
        for parameter in &constructor.value_parameters {
            push_message(&mut body, 2, &encode_value_parameter(parameter));
        }
        push_message(&mut out, 8, &body);
    }
    for function in &class.functions {
        push_message(&mut out, 9, &encode_function(function));
    }
    for property in &class.properties {
        push_message(&mut out, 10, &encode_property(property));
    }
    out
}

/// A `Class` body with the class's own type table appended, which is where the ids its members carry
/// resolve. The table comes after the members and the file index after the table, as the reference
/// compiler writes them.
pub fn encode_class_with_table(class: &ClassMeta, types: &TypeTable) -> Vec<u8> {
    let mut out = encode_class(class);
    if !types.is_empty() {
        push_message(&mut out, 30, &types.encode());
    }
    if let Some(file) = class.file {
        push_varint_field(&mut out, 175, u64::from(file));
    }
    out
}

/// A `Package` body: a file's top-level declarations, the type table their ids resolve in, and the
/// KLIB extension field 171 that travels INSIDE the message.
///
/// 171 being inside the `Package` rather than beside it in the fragment is not a detail a schema
/// makes obvious, and it costs exactly three bytes: a fragment written with it one level out is the
/// right total length with the wrong `Package` length, which is how the byte diff caught it.
pub fn encode_package(
    functions: &[FunctionMeta],
    properties: &[PropertyMeta],
    types: &TypeTable,
    field_171: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    for function in functions {
        push_message(&mut out, 3, &encode_function(function));
    }
    for property in properties {
        push_message(&mut out, 4, &encode_property(property));
    }
    if !types.is_empty() {
        push_message(&mut out, 30, &types.encode());
    }
    push_varint_field(&mut out, 171, field_171);
    out
}

/// Intern a source file name, returning its string-table index.
pub fn intern_file(strings: &mut StringTable, file: &str) -> u32 {
    strings.intern(file)
}
