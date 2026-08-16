//! Shared Kotlin-metadata type and string-table encoder.
//!
//! A `Type` protobuf has the same schema inside packages, classes, properties, constructors,
//! contracts, and type aliases. Keep that schema here; the surrounding declaration builders only
//! decide which field contains the encoded type.

use std::collections::HashMap;
use std::fmt;

use crate::metadata::{protobuf::Pb, serialize_string_table_types};
use crate::spelling::Spelled;
use crate::types::{Ty, TypeName};

/// kotlinc's `JvmNameResolverBase.PREDEFINED_STRINGS`, indexed by `Record.predefined_index`.
/// This is also the decoder's canonical table; a test below prevents the two directions drifting.
pub(crate) const PREDEFINED_STRINGS: &[&str] = &[
    "kotlin/Any",
    "kotlin/Nothing",
    "kotlin/Unit",
    "kotlin/Throwable",
    "kotlin/Number",
    "kotlin/Byte",
    "kotlin/Double",
    "kotlin/Float",
    "kotlin/Int",
    "kotlin/Long",
    "kotlin/Short",
    "kotlin/Boolean",
    "kotlin/Char",
    "kotlin/CharSequence",
    "kotlin/String",
    "kotlin/Comparable",
    "kotlin/Enum",
    "kotlin/Array",
    "kotlin/ByteArray",
    "kotlin/DoubleArray",
    "kotlin/FloatArray",
    "kotlin/IntArray",
    "kotlin/LongArray",
    "kotlin/ShortArray",
    "kotlin/BooleanArray",
    "kotlin/CharArray",
    "kotlin/Cloneable",
    "kotlin/Annotation",
    "kotlin/collections/Iterable",
    "kotlin/collections/MutableIterable",
    "kotlin/collections/Collection",
    "kotlin/collections/MutableCollection",
    "kotlin/collections/List",
    "kotlin/collections/MutableList",
    "kotlin/collections/Set",
    "kotlin/collections/MutableSet",
    "kotlin/collections/Map",
    "kotlin/collections/MutableMap",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/MutableMap.MutableEntry",
    "kotlin/collections/Iterator",
    "kotlin/collections/MutableIterator",
    "kotlin/collections/ListIterator",
    "kotlin/collections/MutableListIterator",
];

#[derive(Default)]
pub(crate) struct StringTable {
    strings: Vec<String>,
    records: Vec<Pb>,
    dedup: HashMap<(String, Vec<u8>), u32>,
    /// Indices of LOCAL class-name strings (`StringTableTypes.localName`, packed field 5): the
    /// string is the RAW internal name of a local/anonymous class, used as a class id verbatim.
    local_names: Vec<u32>,
}

impl StringTable {
    fn intern(&mut self, string: String, record: Pb) -> u32 {
        let key = (string.clone(), record.as_bytes().to_vec());
        if let Some(&index) = self.dedup.get(&key) {
            return index;
        }
        let index = self.strings.len() as u32;
        self.strings.push(string);
        self.records.push(record);
        self.dedup.insert(key, index);
        index
    }

    pub(crate) fn local(&mut self, string: &str) -> u32 {
        self.intern(string.to_owned(), Pb::new())
    }

    pub(crate) fn builtin(&mut self, predefined: usize) -> u32 {
        let mut record = Pb::new();
        record.field_varint(2, predefined as u64);
        self.intern(String::new(), record)
    }

    pub(crate) fn class_id(&mut self, classifier: TypeName) -> u32 {
        if let Some(predefined) = predefined_index(classifier) {
            return self.builtin(predefined);
        }
        let mut record = Pb::new();
        record.field_varint(3, 2); // DESC_TO_CLASS_ID
        self.intern(format!("L{};", classifier.render()), record)
    }

    pub(crate) fn class_id_from_desc(&mut self, descriptor: &str) -> u32 {
        let mut record = Pb::new();
        record.field_varint(3, 2); // DESC_TO_CLASS_ID
        self.intern(descriptor.to_owned(), record)
    }

    pub(crate) fn serialize_types(&self) -> Pb {
        serialize_string_table_types(&self.records, &self.local_names)
    }

    /// Intern a LOCAL/ANONYMOUS class's RAW internal name as a class id: an EMPTY record plus a
    /// `StringTableTypes.localName` entry marking the index (kotlinc's local-class encoding).
    pub(crate) fn local_class_id(&mut self, internal: &str) -> u32 {
        let index = self.intern(internal.to_string(), Pb::new());
        if !self.local_names.contains(&index) {
            self.local_names.push(index);
        }
        index
    }

    pub(crate) fn into_strings(self) -> Vec<String> {
        self.strings
    }
}

fn predefined_index(classifier: TypeName) -> Option<usize> {
    PREDEFINED_STRINGS
        .iter()
        .position(|candidate| classifier.matches(candidate))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TypeEncodeError {
    MissingTypeParameter(String),
    NonMetadataType(Ty),
    FunctionArity(usize),
}

impl fmt::Display for TypeEncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTypeParameter(name) => {
                write!(
                    f,
                    "type parameter '{name}' is absent from the declaration table"
                )
            }
            Self::NonMetadataType(ty) => {
                write!(
                    f,
                    "semantic type '{}' cannot appear in Kotlin metadata",
                    ty.source_name()
                )
            }
            Self::FunctionArity(arity) => {
                write!(f, "function type arity {arity} has no metadata classifier")
            }
        }
    }
}

pub(crate) type TypeParameters = HashMap<String, u64>;

/// Marker bit on a [`TypeParameters`] id: the parameter is CAPTURED from an enclosing class. Its
/// `Type` reference then also records `type_parameter_name` (f9) — the isolated reader has no
/// enclosing-chain context to resolve a bare joint index. In-scope parameters emit f7 alone,
/// matching kotlinc's bytes (kotlinc never writes both for them).
pub(crate) const CAPTURED_TYPE_PARAMETER: u64 = 1 << 32;

/// Marker bit on a [`TypeParameters`] entry whose metadata reference is name-based. Package-level
/// declarations use `Type.type_parameter_name` (f9), unlike class/member declarations whose
/// in-scope parameter tables are addressed by `Type.type_parameter` (f7).
pub(crate) const NAMED_TYPE_PARAMETER: u64 = 1 << 33;

#[derive(Clone, Debug, Default)]
pub struct MetadataTypeParameter {
    pub name: String,
    pub reified: bool,
    pub variance: crate::types::TypeVariance,
    pub upper_bounds: Vec<Ty>,
    /// How source spelled each of `upper_bounds` — `<T : Cargo>` records the alias on the bound's
    /// `Type`. Parallel to `upper_bounds`; a short or empty vec leaves the rest unabbreviated.
    pub upper_bound_spellings: Vec<Spelled>,
}

pub(crate) fn type_parameters<'a>(names: impl Iterator<Item = &'a str>) -> TypeParameters {
    names
        .enumerate()
        .map(|(index, name)| (name.to_owned(), index as u64))
        .collect()
}

pub(crate) fn semantic_type_parameters<'a>(
    names: impl Iterator<Item = &'a str>,
    semantic_names: impl Iterator<Item = &'a str>,
) -> TypeParameters {
    names
        .zip(semantic_names)
        .enumerate()
        .flat_map(|(index, (name, semantic))| {
            [
                (name.to_owned(), index as u64),
                (semantic.to_owned(), index as u64),
            ]
        })
        .collect()
}

pub(crate) fn semantic_named_type_parameters<'a>(
    names: impl Iterator<Item = &'a str>,
    semantic_names: impl Iterator<Item = &'a str>,
) -> TypeParameters {
    names
        .zip(semantic_names)
        .enumerate()
        .flat_map(|(index, (name, semantic))| {
            let value = index as u64 | NAMED_TYPE_PARAMETER;
            [(name.to_owned(), value), (semantic.to_owned(), value)]
        })
        .collect()
}

/// Encode a type whose source spelling cannot name a `typealias` — a synthesized member's
/// signature, a builtin classifier, a contract conclusion. Declared types reachable from source
/// must use [`encode_declared_type`] so an alias spelling becomes `Type.abbreviated_type`.
pub(crate) fn encode_type(
    strings: &mut StringTable,
    ty: Ty,
    type_parameters: &TypeParameters,
) -> Result<Pb, TypeEncodeError> {
    encode_type_with_parameter(
        strings,
        ty,
        Spelled::NONE,
        type_parameters,
        None,
        Expansion::Expanded,
    )
}

/// Encode a DECLARED type together with how source spelled it, so a `typealias` at any node of the
/// tree is recorded as `Type.abbreviated_type` (field 13) next to its expanded classifier.
pub(crate) fn encode_declared_type(
    strings: &mut StringTable,
    ty: Ty,
    spelled: &Spelled,
    type_parameters: &TypeParameters,
) -> Result<Pb, TypeEncodeError> {
    encode_type_with_parameter(
        strings,
        ty,
        spelled,
        type_parameters,
        None,
        Expansion::Expanded,
    )
}

/// Encode a type AS SOURCE WROTE IT: every node that named a `typealias` becomes a bare
/// `Type.type_alias_name` reference instead of its expanded classifier, recursively, and no node
/// carries an `abbreviated_type` (there is nothing to abbreviate — the whole tree is the spelling).
///
/// This is `TypeAlias.underlying_type` (field 4). kotlinc writes the alias's right-hand side there
/// exactly as written and its fully expanded form in `expanded_type` (field 6), so
/// `typealias CargoBox = PBox<Cargo, Cargo>` records `PBox<Cargo, Cargo>` in f4 — argument nodes
/// included — and `PBox<Payload, Payload>` with per-argument abbreviations in f6.
pub(crate) fn encode_spelled_type(
    strings: &mut StringTable,
    ty: Ty,
    spelled: &Spelled,
    type_parameters: &TypeParameters,
) -> Result<Pb, TypeEncodeError> {
    encode_type_with_parameter(
        strings,
        ty,
        spelled,
        type_parameters,
        None,
        Expansion::AsSpelled,
    )
}

/// Which of the two forms of a declared type an encode call is producing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expansion {
    /// The expanded classifier, with any `typealias` spelling recorded alongside as
    /// `Type.abbreviated_type`. Every declared type in a signature takes this form.
    Expanded,
    /// The spelling itself: aliases stay alias references and nothing is abbreviated. Only
    /// `TypeAlias.underlying_type` takes this form.
    AsSpelled,
}

pub(crate) fn encode_indexed_type_parameter(
    strings: &mut StringTable,
    ty: Ty,
    index: u32,
) -> Result<Pb, TypeEncodeError> {
    encode_type_with_parameter(
        strings,
        ty,
        Spelled::NONE,
        &TypeParameters::new(),
        Some(index as u64),
        Expansion::Expanded,
    )
}

fn encode_type_with_parameter(
    strings: &mut StringTable,
    ty: Ty,
    spelled: &Spelled,
    type_parameters: &TypeParameters,
    forced_parameter: Option<u64>,
    expansion: Expansion,
) -> Result<Pb, TypeEncodeError> {
    // As-spelled, an aliased node IS the alias reference — its expanded classifier never appears.
    if expansion == Expansion::AsSpelled && spelled.alias.is_some() {
        if let Some(reference) = encode_alias_reference(
            strings,
            spelled,
            ty.is_nullable(),
            type_parameters,
            // `underlying_type` is the spelling all the way down.
            Expansion::AsSpelled,
        )? {
            return Ok(reference);
        }
    }
    let (nullable, flexible, base) = match ty {
        Ty::Nullable(inner) => (true, false, *inner),
        Ty::PlatformNullable(inner) => (false, true, *inner),
        other => (false, false, other),
    };
    let mut message = Pb::new();
    if flexible {
        let capability = strings.local("kotlin.jvm.PlatformType");
        let upper = encode_type_with_parameter(
            strings,
            Ty::nullable(base),
            spelled,
            type_parameters,
            forced_parameter,
            expansion,
        )?;
        message.field_varint(4, capability as u64);
        message.field_message(5, &upper);
    }
    if let Some(index) = forced_parameter {
        if nullable {
            message.field_varint(3, 1);
        }
        message.field_varint(7, index);
        return Ok(message);
    }

    match base {
        Ty::TyParam(name, _) => {
            let index = type_parameters
                .get(name)
                .copied()
                .ok_or_else(|| TypeEncodeError::MissingTypeParameter(name.to_owned()))?;
            if nullable {
                message.field_varint(3, 1);
            }
            // `type_parameter` (f7) ALONE for an in-scope parameter — kotlinc writes either the
            // table index or a `type_parameter_name` (f9), never both. A CAPTURED enclosing-class
            // parameter also records f9: the reader has no enclosing-chain context for its bare
            // joint index.
            if index & NAMED_TYPE_PARAMETER != 0 {
                let source_name = crate::types::type_parameter_source_name(name);
                message.field_varint(9, strings.local(source_name) as u64);
            } else {
                message.field_varint(7, index & !(CAPTURED_TYPE_PARAMETER | NAMED_TYPE_PARAMETER));
            }
            if index & CAPTURED_TYPE_PARAMETER != 0 {
                let source_name = crate::types::type_parameter_source_name(name);
                message.field_varint(9, strings.local(source_name) as u64);
            }
        }
        Ty::Obj(classifier, arguments) => {
            // kotlinc interns the enclosing classifier before recursively interning its arguments,
            // even though protobuf field order writes `argument` before `class_name`.
            let classifier = strings.class_id(classifier);
            encode_arguments(
                &mut message,
                strings,
                arguments,
                spelled,
                type_parameters,
                expansion,
            )?;
            if nullable {
                message.field_varint(3, 1);
            }
            message.field_varint(6, classifier as u64);
            if expansion == Expansion::Expanded {
                encode_abbreviation(&mut message, strings, spelled, nullable, type_parameters)?;
            }
        }
        Ty::Unit => {
            encode_classifier(&mut message, strings, "kotlin/Unit", nullable);
            encode_abbreviation(&mut message, strings, spelled, nullable, type_parameters)?;
        }
        Ty::Nothing => {
            encode_classifier(&mut message, strings, "kotlin/Nothing", nullable);
            encode_abbreviation(&mut message, strings, spelled, nullable, type_parameters)?;
        }
        Ty::Fun(signature) => {
            let arity = signature.params.len() + usize::from(signature.suspend);
            if arity > 22 {
                return Err(TypeEncodeError::FunctionArity(arity));
            }
            let classifier = crate::types::type_name(&format!("kotlin/Function{arity}"));
            let classifier = strings.class_id(classifier);
            let mut arguments = signature.params.clone();
            if signature.suspend {
                arguments.push(Ty::obj_args(
                    "kotlin/coroutines/Continuation",
                    &[signature.ret],
                ));
                arguments.push(Ty::nullable(Ty::obj("kotlin/Any")));
            } else {
                arguments.push(signature.ret);
            }
            // A function type's metadata arguments are SYNTHESIZED (`params… + ret`, or the CPS
            // `Continuation`/`Any?` pair for a suspend one). `spelling_of_ref` lays an arrow type's
            // component spellings out in that same order, so they zip positionally; a spelling that
            // came from a NAMED alias has no argument list and contributes nothing here, which is
            // right — its arguments belong to the abbreviation, not to the expansion.
            encode_arguments(
                &mut message,
                strings,
                &arguments,
                spelled,
                type_parameters,
                expansion,
            )?;
            if nullable {
                message.field_varint(3, 1);
            }
            message.field_varint(6, classifier as u64);
            if expansion == Expansion::Expanded {
                encode_abbreviation(&mut message, strings, spelled, nullable, type_parameters)?;
            }
            if signature.has_receiver {
                add_extension_function_annotation(&mut message, strings);
            }
            if signature.suspend {
                message.field_varint(1, 1); // Type.flags: SUSPEND_TYPE
            }
        }
        Ty::Null
        | Ty::Error
        | Ty::Nullable(_)
        | Ty::PlatformNullable(_)
        | Ty::InProjection(_)
        | Ty::OutProjection(_) => return Err(TypeEncodeError::NonMetadataType(ty)),
    }
    Ok(message)
}

fn encode_classifier(
    message: &mut Pb,
    strings: &mut StringTable,
    classifier: &str,
    nullable: bool,
) {
    if nullable {
        message.field_varint(3, 1);
    }
    let index = PREDEFINED_STRINGS
        .iter()
        .position(|candidate| *candidate == classifier)
        .expect("canonical Kotlin metadata classifier must be predefined");
    message.field_varint(6, strings.builtin(index) as u64);
}

fn encode_arguments(
    message: &mut Pb,
    strings: &mut StringTable,
    arguments: &[Ty],
    spelled: &Spelled,
    type_parameters: &TypeParameters,
    expansion: Expansion,
) -> Result<(), TypeEncodeError> {
    for (index, argument) in arguments.iter().enumerate() {
        let (projection, argument) = match argument {
            Ty::InProjection(inner) => (Some(0), **inner),
            Ty::OutProjection(inner) => (Some(1), **inner),
            ordinary => (None, *ordinary),
        };
        let mut encoded_argument = Pb::new();
        if let Some(projection) = projection {
            encoded_argument.field_varint(1, projection);
        }
        // A use-site projection wraps the argument; the abbreviation belongs to the type INSIDE it
        // (`List<out Cargo>` records `Argument{projection, type={Payload, abbreviated=Cargo}}`).
        encoded_argument.field_message(
            2,
            &encode_type_with_parameter(
                strings,
                argument,
                spelled.arg(index),
                type_parameters,
                None,
                expansion,
            )?,
        );
        message.repeated_message(2, &encoded_argument);
    }
    Ok(())
}

/// A `Type` that names a `typealias` DIRECTLY through `Type.type_alias_name` (field 12) with no
/// `class_name` — the shape of both `Type.abbreviated_type`'s payload and a
/// `TypeAlias.underlying_type` whose right-hand side spells another alias.
///
/// Its arguments are the AS-SPELLED ones, which is why they come from `alias_args` rather than
/// from any expanded `Ty`. Returns `None` when nothing was spelled as an alias here.
pub(crate) fn encode_alias_reference(
    strings: &mut StringTable,
    spelled: &Spelled,
    nullable: bool,
    type_parameters: &TypeParameters,
    arguments: Expansion,
) -> Result<Option<Pb>, TypeEncodeError> {
    let Some(alias) = spelled.alias else {
        return Ok(None);
    };
    // Like every other `Type`, the classifier reference interns BEFORE its arguments — the rule
    // that keeps `d2` in kotlinc's order.
    let alias_id = strings.class_id(alias);
    let mut message = Pb::new();
    for (ty, argument_spelling) in &spelled.alias_args {
        let (projection, ty) = match ty {
            Ty::InProjection(inner) => (Some(0), **inner),
            Ty::OutProjection(inner) => (Some(1), **inner),
            ordinary => (None, *ordinary),
        };
        let mut encoded_argument = Pb::new();
        if let Some(projection) = projection {
            encoded_argument.field_varint(1, projection);
        }
        encoded_argument.field_message(
            2,
            &encode_type_with_parameter(
                strings,
                ty,
                argument_spelling,
                type_parameters,
                None,
                arguments,
            )?,
        );
        message.repeated_message(2, &encoded_argument);
    }
    if nullable {
        message.field_varint(3, 1);
    }
    message.field_varint(12, alias_id as u64); // Type.type_alias_name = 12
    Ok(Some(message))
}

/// Append `Type.abbreviated_type` (field 13) when source spelled a `typealias` at this node.
///
/// The abbreviated `Type` is a full message in its own right: it repeats the node's `nullable` flag
/// (a declared `Cargo?` is nullable on BOTH forms), carries the alias's AS-SPELLED arguments — a
/// different arity from the expanded type's — and names the alias through `Type.type_alias_name`
/// (field 12) INSTEAD of `class_name`. Like every other `Type`, its classifier reference interns
/// before its arguments, which is what keeps `d2` in kotlinc's order.
fn encode_abbreviation(
    message: &mut Pb,
    strings: &mut StringTable,
    spelled: &Spelled,
    nullable: bool,
    type_parameters: &TypeParameters,
) -> Result<(), TypeEncodeError> {
    // An ABBREVIATION's own arguments are EXPANDED, each carrying its own abbreviation:
    // `Boxed<Cargo>` writes `abbreviated_type = {argument={Payload, abbreviated={Cargo}}, Boxed}`,
    // NOT a bare `Cargo` reference. This is the one place the two alias-reference forms differ, and
    // it is read off kotlinc 2.4.10 rather than inferred.
    let Some(abbreviated) = encode_alias_reference(
        strings,
        spelled,
        nullable,
        type_parameters,
        Expansion::Expanded,
    )?
    else {
        return Ok(());
    };
    message.field_message(13, &abbreviated); // Type.abbreviated_type = 13
    Ok(())
}

pub(crate) fn add_extension_function_annotation(message: &mut Pb, strings: &mut StringTable) {
    let annotation_id = strings.class_id(crate::types::type_name("kotlin/ExtensionFunctionType"));
    let mut annotation = Pb::new();
    annotation.field_varint(1, annotation_id as u64);
    message.field_message(100, &annotation);
}

pub(crate) fn encode_metadata_type_parameter(
    strings: &mut StringTable,
    index: usize,
    parameter: &MetadataTypeParameter,
    type_parameters: &TypeParameters,
) -> Result<Pb, TypeEncodeError> {
    let mut message = Pb::new();
    message.field_varint(1, index as u64);
    message.field_varint(2, strings.local(&parameter.name) as u64);
    if parameter.reified {
        message.field_varint(3, 1);
    }
    match parameter.variance {
        crate::types::TypeVariance::In => message.field_varint(4, 0),
        crate::types::TypeVariance::Out => message.field_varint(4, 1),
        crate::types::TypeVariance::Invariant => {}
    }
    for (index, bound) in parameter.upper_bounds.iter().enumerate() {
        let spelled = parameter
            .upper_bound_spellings
            .get(index)
            .unwrap_or(Spelled::NONE);
        message.field_message(
            5,
            &encode_declared_type(strings, *bound, spelled, type_parameters)?,
        );
    }
    Ok(message)
}

pub(crate) fn encode_type_parameter(
    strings: &mut StringTable,
    index: usize,
    name: &str,
    reified: bool,
) -> Pb {
    encode_metadata_type_parameter(
        strings,
        index,
        &MetadataTypeParameter {
            name: name.to_owned(),
            reified,
            variance: crate::types::TypeVariance::Invariant,
            upper_bounds: Vec::new(),
            upper_bound_spellings: Vec::new(),
        },
        &TypeParameters::new(),
    )
    .expect("an unbounded type parameter is always metadata-encodable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_table_matches_decoder() {
        assert_eq!(PREDEFINED_STRINGS, crate::jvm::metadata::PREDEFINED_STRINGS);
    }

    #[test]
    fn one_encoder_preserves_nullability_generics_and_projections() {
        let mut strings = StringTable::default();
        let ty = Ty::nullable(Ty::obj_args(
            "sample/Box",
            &[Ty::in_projection(Ty::String), Ty::out_projection(Ty::Int)],
        ));
        let encoded = encode_type(&mut strings, ty, &TypeParameters::new()).unwrap();
        assert_eq!(
            encoded.as_bytes(),
            &[
                0x12, 0x06, 0x08, 0x00, 0x12, 0x02, 0x30, 0x01, // in String
                0x12, 0x06, 0x08, 0x01, 0x12, 0x02, 0x30, 0x02, // out Int
                0x18, 0x01, // nullable
                0x30, 0x00, // sample/Box (interned before its arguments)
            ]
        );
    }

    #[test]
    fn unresolved_type_parameter_is_an_error() {
        let mut strings = StringTable::default();
        let result = encode_type(
            &mut strings,
            Ty::ty_param("T", Ty::obj("kotlin/Any")),
            &TypeParameters::new(),
        );
        let Err(error) = result else {
            panic!("an undeclared type parameter was encoded")
        };
        assert_eq!(error, TypeEncodeError::MissingTypeParameter("T".into()));
    }

    #[test]
    fn platform_type_encodes_a_nullable_flexible_upper_bound() {
        let mut strings = StringTable::default();
        let encoded = encode_type(
            &mut strings,
            Ty::platform_nullable(Ty::obj_args("sample/Box", &[Ty::String])),
            &TypeParameters::new(),
        )
        .unwrap();
        assert!(encoded.as_bytes().starts_with(&[0x20]));
        assert!(encoded.as_bytes().contains(&0x30));
    }
}
