//! Which C declarations an interop library binds, in the order Kotlin/Native's indexer finds
//! them, and the Kotlin name of every struct, union and enum among them.
//!
//! The indexer visits the declarations of the library's own headers in source order. A struct,
//! union, enum or typedef is registered when it is first visited or first referenced from a
//! registered declaration's type, wherever it is declared: a struct before the types of its
//! fields, a typedef after the type it names. Functions and variables come only from the own
//! headers, in declaration order.
//!
//! A typedef that names a struct or enum by that struct's or enum's own name
//! (`typedef struct s s;`, `typedef struct { … } point;`) is transparent: it is never registered,
//! and a use of it is a use of the struct or enum.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::super::model::{
    Declarations, Declared, EnumId, Field, FloatKind, IntRank, Origin, RecordId, Type, TypeId,
    TypedefId,
};

pub(super) struct Registry {
    /// Structs and unions, defined or not, in registration order.
    pub(super) records: Vec<RecordId>,
    pub(super) enums: Vec<EnumId>,
    pub(super) typedefs: Vec<TypedefId>,
    /// Indices into [`Declarations::functions`], in declaration order.
    pub(super) functions: Vec<usize>,
    /// Indices into [`Declarations::variables`], in declaration order.
    pub(super) variables: Vec<usize>,
    record_names: HashMap<RecordId, Rc<str>>,
    enum_names: HashMap<EnumId, Rc<str>>,
    registered_typedefs: HashSet<TypedefId>,
    /// The typedef naming each tagless struct or enum, which gives it that name.
    typedef_names_record: HashMap<RecordId, Rc<str>>,
    typedef_names_enum: HashMap<EnumId, Rc<str>>,
    anonymous_records: u32,
}

impl Registry {
    /// Register what the declarations of `declarations` for which `own` holds reach.
    pub(super) fn new(declarations: &Declarations, own: impl Fn(&Origin) -> bool) -> Self {
        let mut registry = Self {
            records: Vec::new(),
            enums: Vec::new(),
            typedefs: Vec::new(),
            functions: Vec::new(),
            variables: Vec::new(),
            record_names: HashMap::new(),
            enum_names: HashMap::new(),
            registered_typedefs: HashSet::new(),
            typedef_names_record: HashMap::new(),
            typedef_names_enum: HashMap::new(),
            anonymous_records: 0,
        };
        for typedef in &declarations.typedefs {
            match declarations.ty(typedef.ty) {
                Type::Record(id) if declarations.record(*id).tag.is_none() => {
                    registry
                        .typedef_names_record
                        .entry(*id)
                        .or_insert_with(|| typedef.name.clone());
                }
                Type::Enum(id) if declarations.enumeration(*id).tag.is_none() => {
                    registry
                        .typedef_names_enum
                        .entry(*id)
                        .or_insert_with(|| typedef.name.clone());
                }
                _ => {}
            }
        }
        for declared in &declarations.order {
            match *declared {
                Declared::Record(id) => {
                    let record = declarations.record(id);
                    let named =
                        record.tag.is_some() || registry.typedef_names_record.contains_key(&id);
                    if named && own(&record.origin) {
                        registry.record(declarations, id);
                    }
                }
                Declared::Enum(id) => {
                    if own(&declarations.enumeration(id).origin) {
                        registry.enumeration(declarations, id);
                    }
                }
                Declared::Typedef(id) => {
                    if declarations.typedef(id).origin.as_ref().is_some_and(&own) {
                        registry.typedef(declarations, id);
                    }
                }
                Declared::Function(index) => {
                    let function = &declarations.functions[index];
                    if own(&function.origin) {
                        // The parameters as declared: `va_list ap` reaches the `va_list` typedef,
                        // which the adjusted pointer type does not.
                        registry.visit(declarations, function.ty);
                        for &param in &function.param_types {
                            registry.visit(declarations, param);
                        }
                        registry.functions.push(index);
                    }
                }
                Declared::Variable(index) => {
                    let variable = &declarations.variables[index];
                    if own(&variable.origin) {
                        registry.visit(declarations, variable.ty);
                        registry.variables.push(index);
                    }
                }
            }
        }
        registry
    }

    /// The Kotlin name of a registered struct or union.
    pub(super) fn record_name(&self, id: RecordId) -> &Rc<str> {
        &self.record_names[&id]
    }

    /// The Kotlin name of a registered enum; `None` for an anonymous one.
    pub(super) fn enum_name(&self, id: EnumId) -> Option<&Rc<str>> {
        self.enum_names.get(&id)
    }

    /// Whether `id` is registered; a transparent or unsupported typedef is not.
    pub(super) fn has_typedef(&self, id: TypedefId) -> bool {
        self.registered_typedefs.contains(&id)
    }

    fn record(&mut self, declarations: &Declarations, id: RecordId) {
        if self.record_names.contains_key(&id) {
            return;
        }
        let record = declarations.record(id);
        let name = match (&record.tag, self.typedef_names_record.get(&id)) {
            (Some(tag), _) => tag.clone(),
            (None, Some(name)) => name.clone(),
            (None, None) => {
                self.anonymous_records += 1;
                Rc::from(format!("anonymousStruct{}", self.anonymous_records))
            }
        };
        self.record_names.insert(id, name);
        self.records.push(id);
        if let Some(fields) = &record.fields {
            self.fields(declarations, fields);
        }
    }

    /// The types of a record's fields, an anonymous member's own fields in its place.
    fn fields(&mut self, declarations: &Declarations, fields: &[Field]) {
        for field in fields {
            if field.name.is_none() && field.bit_width.is_none() {
                if let Type::Record(inner) = declarations.ty(declarations.canonical(field.ty)) {
                    if let Some(inner_fields) = &declarations.record(*inner).fields {
                        self.fields(declarations, inner_fields);
                    }
                    continue;
                }
            }
            self.visit(declarations, field.ty);
        }
    }

    fn enumeration(&mut self, declarations: &Declarations, id: EnumId) {
        if self.enums.contains(&id) {
            return;
        }
        self.enums.push(id);
        let name = declarations
            .enumeration(id)
            .tag
            .clone()
            .or_else(|| self.typedef_names_enum.get(&id).cloned());
        if let Some(name) = name {
            self.enum_names.insert(id, name);
        }
    }

    /// Register a typedef after the type it names; `false` when that type is unsupported.
    fn typedef(&mut self, declarations: &Declarations, id: TypedefId) -> bool {
        let typedef = declarations.typedef(id);
        if !self.visit(declarations, typedef.ty) {
            return false;
        }
        let mut aliased = typedef.ty;
        while let Type::Qualified { base, .. } = declarations.ty(aliased) {
            aliased = *base;
        }
        let transparent = match declarations.ty(aliased) {
            Type::Record(record) => **self.record_name(*record) == *typedef.name,
            Type::Enum(enumeration) => self
                .enum_name(*enumeration)
                .is_some_and(|name| **name == *typedef.name),
            _ => false,
        };
        if !transparent && self.registered_typedefs.insert(id) {
            self.typedefs.push(id);
        }
        true
    }

    /// Register what `ty` references; `false` when `ty` itself is a type Kotlin/Native does not
    /// convert (a 128-bit integer, `long double`, a complex or atomic type).
    fn visit(&mut self, declarations: &Declarations, ty: TypeId) -> bool {
        match declarations.ty(ty) {
            Type::Void | Type::Vector { .. } => true,
            Type::Int { rank, .. } => *rank != IntRank::Int128,
            Type::Float(kind) => *kind != FloatKind::LongDouble,
            Type::Complex(_) | Type::Atomic(_) => false,
            Type::Pointer(pointee) => {
                self.visit(declarations, *pointee);
                true
            }
            Type::Array { element, .. } => {
                self.visit(declarations, *element);
                true
            }
            Type::Function(function) => {
                self.visit(declarations, function.result);
                for &param in &function.params {
                    self.visit(declarations, param);
                }
                true
            }
            Type::Record(id) => {
                self.record(declarations, *id);
                true
            }
            Type::Enum(id) => {
                self.enumeration(declarations, *id);
                true
            }
            Type::Typedef(id) => self.typedef(declarations, *id),
            Type::Qualified { base, .. } => self.visit(declarations, *base),
        }
    }
}
