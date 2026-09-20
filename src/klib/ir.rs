//! The bodies half of a klib: `default/ir/`, where a parameter's DEFAULT VALUE lives.
//!
//! `linkdata` describes declarations. It records THAT a parameter has a default and never what the
//! default IS — that is an expression, and expressions live here. A call that omits a defaulted
//! argument therefore cannot be compiled from `linkdata` alone: `assertEquals(expected, actual)`
//! needs to know that the third parameter is `null`, and no amount of reading declarations will
//! say so. It is the single largest thing standing between a klib-backed compilation and the
//! corpus.
//!
//! **Scope.** This reads CONSTANTS, and nothing else. A default that is a call, a reference or any
//! other expression is reported as absent rather than approximated — a caller that cannot see the
//! value must decline the call, not guess at it. In the Kotlin standard library the overwhelming
//! majority are `null`, `false`, `""` and small integers, which is why so little buys so much.
//!
//! **Layout.** Six files, each an array with one entry per FILE of the library:
//!
//! ```text
//! [u32 count] [u32 size x count] [payload x count]
//! ```
//!
//! and each payload is itself an array of that file's own entries:
//!
//! ```text
//! [i32 n] [size x |n|] [entry x |n|]
//! ```
//!
//! where a NEGATIVE `n` means the sizes are varints and a non-negative one means they are `u32`s —
//! the writer picks whichever is smaller. `irDeclarations.knd` is the exception: its entries are
//! addressed by a declaration ID rather than by position, so it carries an explicit index of
//! `(id, offset, size)` triples.
//!
//! Every integer outside a protobuf message is BIG-endian.
//!
//! **Encodings.** Two of them are not protobuf's. A name and a type travel together in one `int64`
//! as a Morton code — the two indices' bits interleaved, so that a pair of small numbers stays a
//! short varint. And a `float`/`double` constant is written as its raw bits in a fixed-width field
//! rather than as a protobuf `float`/`double`. Everything else is ordinary protobuf, including the
//! integer constants: they are `int32`/`int64` rather than `sint32`, so a negative default is
//! written sign-extended to ten bytes and must not be read as a zigzag.

use std::collections::HashMap;

use super::{KlibArchive, KlibError};

/// Where a klib keeps its serialized IR, relative to the archive root. Shared with the container
/// reader's own listing so the two cannot name different directories.
use super::IR_PREFIX;

/// A constant a parameter defaults to.
///
/// Deliberately this module's own type rather than the compiler's: the container reader depends on
/// nothing in the compiler, and what a decoded constant MEANS — which Kotlin type it fills, how a
/// target materializes it — is the caller's to decide.
#[derive(Clone, Debug, PartialEq)]
pub enum IrConstant {
    Null,
    Boolean(bool),
    Char(u16),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    String(String),
}

/// The declaration a set of defaults belongs to, in the terms a caller reading `linkdata` has.
///
/// Matching is by NAME and shape rather than by the library's own `IdSignature`, which would have
/// to be reconstructed from metadata to be compared. The parameter names carry most of the
/// discrimination: two overloads sharing a name, an arity AND every parameter name are rare, and
/// where they occur [`IrDefaults`] refuses to answer rather than choose between them.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IrDefaultKey {
    /// The package, dotted (`kotlin.test`), empty for the root package.
    pub package: String,
    /// The enclosing classifiers, outermost first, empty for a top-level declaration.
    pub owners: Vec<String>,
    /// The declaration's own name (`assertEquals`, `<init>`).
    pub name: String,
    /// The value parameters' source names, in declaration order.
    pub parameters: Vec<String>,
}

/// Every declaration's parameter defaults, read out of one klib's IR.
#[derive(Default)]
pub struct IrDefaults {
    /// `None` marks a key several declarations answered DIFFERENTLY. Two overloads that match on
    /// name, arity and every parameter name are indistinguishable here, so a key they disagree on
    /// is published as unknown — which makes a caller decline the call rather than pass a value
    /// taken from the wrong declaration.
    known: HashMap<IrDefaultKey, Option<Vec<Option<IrConstant>>>>,
}

impl IrDefaults {
    /// The defaults of the declaration this key names, parallel to its parameters, or `None` when
    /// the library does not say — a default that is not a constant, or a key more than one
    /// declaration answers differently.
    pub fn get(&self, key: &IrDefaultKey) -> Option<&[Option<IrConstant>]> {
        self.known.get(key)?.as_deref()
    }

    /// How many declarations answered, for a caller that wants to say the IR really was read.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    fn record(&mut self, key: IrDefaultKey, values: Vec<Option<IrConstant>>) {
        match self.known.get(&key) {
            None => {
                self.known.insert(key, Some(values));
            }
            Some(Some(existing)) if *existing == values => {}
            Some(_) => {
                self.known.insert(key, None);
            }
        }
    }
}

/// What can go wrong reading the IR. Each names the file it was reading: a truncated or
/// unexpectedly shaped container is a broken toolchain rather than a program error.
#[derive(Debug)]
pub enum IrError {
    /// The archive has no serialized IR where one must be.
    Missing { entry: String },
    /// The container would not open, or an entry would not read.
    Container(KlibError),
    /// A table did not have the shape the format specifies.
    Malformed { entry: String, detail: String },
}

impl std::fmt::Display for IrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { entry } => write!(f, "the klib has no serialized IR at {entry}"),
            Self::Container(error) => write!(f, "invalid klib IR container: {error}"),
            Self::Malformed { entry, detail } => write!(f, "invalid klib IR {entry}: {detail}"),
        }
    }
}

impl From<KlibError> for IrError {
    fn from(error: KlibError) -> Self {
        Self::Container(error)
    }
}

/// Read every parameter default this klib's IR states as a constant.
pub fn read_defaults(archive: &KlibArchive) -> Result<IrDefaults, IrError> {
    let files = per_file(archive, "files.knf")?;
    let strings = per_file(archive, "strings.knt")?;
    let declarations = per_file(archive, "irDeclarations.knd")?;
    let bodies = per_file(archive, "bodies.knb")?;
    if strings.len() != files.len()
        || declarations.len() != files.len()
        || bodies.len() != files.len()
    {
        return Err(IrError::Malformed {
            entry: "ir".to_string(),
            detail: format!(
                "the per-file tables disagree on how many files there are: \
                 {} files, {} string tables, {} declaration tables, {} body tables",
                files.len(),
                strings.len(),
                declarations.len(),
                bodies.len()
            ),
        });
    }
    let mut defaults = IrDefaults::default();
    for index in 0..files.len() {
        let strings = entries(&strings[index], "strings.knt")?
            .into_iter()
            .map(|entry| String::from_utf8_lossy(entry).into_owned())
            .collect::<Vec<_>>();
        let bodies = entries(&bodies[index], "bodies.knb")?;
        let declarations = declaration_index(&declarations[index])?;
        let file = File {
            strings: &strings,
            bodies: &bodies,
        };
        // `IrFile.fq_name` is field 3: the package's segments as string ids.
        let package = file.qualified(&packed(&files[index], 3))?;
        // `IrFile.declaration_id` is field 1: the file's top-level declarations, by ID.
        for id in packed(&files[index], 1) {
            let Some(body) = declarations.get(&id) else {
                continue;
            };
            walk(&file, body, &package, &mut Vec::new(), &mut defaults)?;
        }
    }
    Ok(defaults)
}

/// One file's own tables. Every index a declaration carries is into THESE, not into the library's.
struct File<'a> {
    strings: &'a [String],
    bodies: &'a [&'a [u8]],
}

impl File<'_> {
    fn name(&self, id: usize) -> Result<&str, IrError> {
        self.strings
            .get(id)
            .map(String::as_str)
            .ok_or_else(|| IrError::Malformed {
                entry: "strings.knt".to_string(),
                detail: format!("a declaration references absent string {id}"),
            })
    }

    fn qualified(&self, ids: &[u64]) -> Result<String, IrError> {
        let mut segments = Vec::with_capacity(ids.len());
        for id in ids {
            segments.push(self.name(*id as usize)?.to_string());
        }
        Ok(segments.join("."))
    }
}

/// Walk one declaration, recording what it and anything nested inside it default to.
fn walk(
    file: &File<'_>,
    declaration: &[u8],
    package: &str,
    owners: &mut Vec<String>,
    into: &mut IrDefaults,
) -> Result<(), IrError> {
    for (field, wire, value) in message(declaration) {
        let Value::Bytes(body) = value else { continue };
        if wire != 2 {
            continue;
        }
        match field {
            // `IrDeclaration.ir_class`: recurse, with this class on the owner path.
            2 => {
                let name = first_varint(body, 2).ok_or_else(|| IrError::Malformed {
                    entry: "irDeclarations.knd".to_string(),
                    detail: "a class declaration has no name".to_string(),
                })?;
                owners.push(file.name(name as usize)?.to_string());
                for (nested_field, nested_wire, nested) in message(body) {
                    if nested_field == 5 && nested_wire == 2 {
                        if let Value::Bytes(nested) = nested {
                            walk(file, nested, package, owners, into)?;
                        }
                    }
                }
                owners.pop();
            }
            // `ir_constructor` and `ir_function`: both a bare `IrFunctionBase` under field 1.
            3 | 6 => {
                if let Some(Value::Bytes(base)) = first(body, 1) {
                    function(file, base, package, owners, into)?;
                }
            }
            // `ir_property`: its accessors are `IrFunction`s of their own.
            7 => {
                for (accessor, accessor_wire, value) in message(body) {
                    if matches!(accessor, 4 | 5) && accessor_wire == 2 {
                        if let Value::Bytes(function_body) = value {
                            if let Some(Value::Bytes(base)) = first(function_body, 1) {
                                function(file, base, package, owners, into)?;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Record one `IrFunctionBase`'s parameter defaults.
fn function(
    file: &File<'_>,
    base: &[u8],
    package: &str,
    owners: &[String],
    into: &mut IrDefaults,
) -> Result<(), IrError> {
    let Some(Value::Varint(name_type)) = first(base, 2) else {
        return Ok(());
    };
    let (name, _) = morton(name_type);
    let mut parameters = Vec::new();
    let mut values = Vec::new();
    let mut any = false;
    for (field, wire, value) in message(base) {
        // `IrFunctionBase.regular_parameter`. The dispatch and extension receivers are fields 4
        // and 5 and carry no default, so only this one is read.
        if field != 6 || wire != 2 {
            continue;
        }
        let Value::Bytes(parameter) = value else {
            continue;
        };
        let Some(Value::Varint(parameter_name_type)) = first(parameter, 2) else {
            return Ok(());
        };
        let (parameter_name, _) = morton(parameter_name_type);
        parameters.push(file.name(parameter_name as usize)?.to_string());
        let constant = match first(parameter, 4) {
            Some(Value::Varint(body)) => {
                any = true;
                file.bodies
                    .get(body as usize)
                    .and_then(|body| constant(file, body))
            }
            _ => None,
        };
        values.push(constant);
    }
    // A declaration with no defaulted parameter at all has nothing to say; recording it would only
    // grow the table with keys no caller can ask about.
    if !any {
        return Ok(());
    }
    into.record(
        IrDefaultKey {
            package: package.to_string(),
            owners: owners.to_vec(),
            name: file.name(name as usize)?.to_string(),
            parameters,
        },
        values,
    );
    Ok(())
}

/// The constant an expression body states, or `None` for a default that is not one.
fn constant(file: &File<'_>, body: &[u8]) -> Option<IrConstant> {
    // `IrExpression.op_const` is field 5. Anything else — a call, a reference, an `if` — is an
    // expression this module deliberately does not evaluate.
    let Value::Bytes(literal) = first(body, 5)? else {
        return None;
    };
    let (field, _, value) = message(literal).next()?;
    Some(match (field, value) {
        (1, Value::Varint(_)) => IrConstant::Null,
        (2, Value::Varint(value)) => IrConstant::Boolean(value != 0),
        // Each of these is a protobuf `int32`/`int64`, NOT a `sint32`: a negative one is written
        // sign-extended to ten bytes rather than zigzagged. `joinToString(limit: Int = -1)` is the
        // declaration that says so — read as zigzag it came out as `0`.
        (3, Value::Varint(value)) => IrConstant::Char(value as i64 as i32 as u16),
        (4, Value::Varint(value)) => IrConstant::Byte(value as i64 as i8),
        (5, Value::Varint(value)) => IrConstant::Short(value as i64 as i16),
        (6, Value::Varint(value)) => IrConstant::Int(value as i64 as i32),
        (7, Value::Varint(value)) => IrConstant::Long(value as i64),
        (8, Value::Fixed32(bits)) => IrConstant::Float(f32::from_bits(bits)),
        (9, Value::Fixed64(bits)) => IrConstant::Double(f64::from_bits(bits)),
        (10, Value::Varint(id)) => IrConstant::String(file.strings.get(id as usize)?.clone()),
        _ => return None,
    })
}

/// `BinaryLattice`: two indices with their bits interleaved, so a pair of small numbers stays a
/// short varint. The first takes the even bits and the second the odd ones.
fn morton(code: u64) -> (u64, u64) {
    fn compact(mut x: u64) -> u64 {
        x &= 0x5555_5555_5555_5555;
        x = (x ^ (x >> 1)) & 0x3333_3333_3333_3333;
        x = (x ^ (x >> 2)) & 0x0f0f_0f0f_0f0f_0f0f;
        x = (x ^ (x >> 4)) & 0x00ff_00ff_00ff_00ff;
        x = (x ^ (x >> 8)) & 0x0000_ffff_0000_ffff;
        (x ^ (x >> 16)) & 0xffff_ffff
    }
    (compact(code), compact(code >> 1))
}

/// One decoded protobuf field's payload.
enum Value<'a> {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Bytes(&'a [u8]),
}

/// Walk a protobuf message's fields. A malformed message simply ends: this module reads a library
/// the toolchain produced, and every consumer treats an unreadable expression as "no constant".
fn message(body: &[u8]) -> impl Iterator<Item = (u64, u64, Value<'_>)> {
    let mut offset = 0usize;
    std::iter::from_fn(move || {
        let (tag, next) = varint(body, offset)?;
        offset = next;
        let (field, wire) = (tag >> 3, tag & 7);
        let value = match wire {
            0 => {
                let (value, next) = varint(body, offset)?;
                offset = next;
                Value::Varint(value)
            }
            1 => {
                let bytes = body.get(offset..offset + 8)?;
                offset += 8;
                Value::Fixed64(u64::from_le_bytes(bytes.try_into().ok()?))
            }
            2 => {
                let (length, next) = varint(body, offset)?;
                let length = usize::try_from(length).ok()?;
                let bytes = body.get(next..next.checked_add(length)?)?;
                offset = next + length;
                Value::Bytes(bytes)
            }
            5 => {
                let bytes = body.get(offset..offset + 4)?;
                offset += 4;
                Value::Fixed32(u32::from_le_bytes(bytes.try_into().ok()?))
            }
            _ => return None,
        };
        Some((field, wire, value))
    })
}

fn first(body: &[u8], field: u64) -> Option<Value<'_>> {
    message(body)
        .find(|(number, _, _)| *number == field)
        .map(|(_, _, value)| value)
}

fn first_varint(body: &[u8], field: u64) -> Option<u64> {
    match first(body, field)? {
        Value::Varint(value) => Some(value),
        _ => None,
    }
}

/// A packed repeated varint field, as `IrFile` writes both its declaration ids and its package.
fn packed(body: &[u8], field: u64) -> Vec<u64> {
    let Some(Value::Bytes(bytes)) = first(body, field) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some((value, next)) = varint(bytes, offset) {
        out.push(value);
        offset = next;
    }
    out
}

fn varint(body: &[u8], mut offset: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *body.get(offset)?;
        offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some((value, offset));
        }
    }
    None
}

/// The per-FILE array one of the six IR files is: `[u32 count][u32 size x count][payload x count]`.
fn per_file(archive: &KlibArchive, name: &str) -> Result<Vec<Vec<u8>>, IrError> {
    let entry = format!("{IR_PREFIX}{name}");
    if !archive
        .entries()
        .iter()
        .any(|candidate| candidate == &entry)
    {
        return Err(IrError::Missing { entry });
    }
    let bytes = archive.read(&entry)?;
    let malformed = |detail: String| IrError::Malformed {
        entry: name.to_string(),
        detail,
    };
    let count =
        u32_at(&bytes, 0).ok_or_else(|| malformed("truncated header".to_string()))? as usize;
    let mut sizes = Vec::with_capacity(count);
    for index in 0..count {
        sizes.push(
            u32_at(&bytes, 4 + 4 * index)
                .ok_or_else(|| malformed(format!("truncated size {index}")))? as usize,
        );
    }
    let mut offset = 4 + 4 * count;
    let mut out = Vec::with_capacity(count);
    for (index, size) in sizes.into_iter().enumerate() {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| malformed(format!("oversized entry {index}")))?;
        out.push(
            bytes
                .get(offset..end)
                .ok_or_else(|| malformed(format!("truncated entry {index}")))?
                .to_vec(),
        );
        offset = end;
    }
    Ok(out)
}

/// One file's own array: `[i32 n][size x |n|][entry x |n|]`, where a NEGATIVE `n` means the sizes
/// are varints and a non-negative one means they are `u32`s.
fn entries<'a>(bytes: &'a [u8], name: &str) -> Result<Vec<&'a [u8]>, IrError> {
    let malformed = |detail: String| IrError::Malformed {
        entry: name.to_string(),
        detail,
    };
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let raw = u32_at(bytes, 0).ok_or_else(|| malformed("truncated header".to_string()))? as i32;
    let mut offset = 4;
    let mut sizes = Vec::new();
    if raw < 0 {
        let count = raw.unsigned_abs() as usize;
        for index in 0..count {
            let (size, next) = varint(bytes, offset)
                .ok_or_else(|| malformed(format!("truncated size {index}")))?;
            sizes.push(size as usize);
            offset = next;
        }
    } else {
        let count = raw as usize;
        for index in 0..count {
            sizes.push(
                u32_at(bytes, offset).ok_or_else(|| malformed(format!("truncated size {index}")))?
                    as usize,
            );
            offset += 4;
        }
    }
    let mut out = Vec::with_capacity(sizes.len());
    for (index, size) in sizes.into_iter().enumerate() {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| malformed(format!("oversized entry {index}")))?;
        out.push(
            bytes
                .get(offset..end)
                .ok_or_else(|| malformed(format!("truncated entry {index}")))?,
        );
        offset = end;
    }
    Ok(out)
}

/// `irDeclarations.knd`'s own shape: its entries are addressed by declaration ID, so it carries an
/// index of `(id, offset, size)` triples rather than a run of sizes.
fn declaration_index(bytes: &[u8]) -> Result<HashMap<u64, &[u8]>, IrError> {
    let malformed = |detail: String| IrError::Malformed {
        entry: "irDeclarations.knd".to_string(),
        detail,
    };
    if bytes.is_empty() {
        return Ok(HashMap::new());
    }
    let count = u32_at(bytes, 0).ok_or_else(|| malformed("truncated header".to_string()))? as usize;
    let mut out = HashMap::with_capacity(count);
    for index in 0..count {
        let at = 4 + 12 * index;
        let id = u32_at(bytes, at).ok_or_else(|| malformed(format!("truncated id {index}")))?;
        let offset = u32_at(bytes, at + 4)
            .ok_or_else(|| malformed(format!("truncated offset {index}")))?
            as usize;
        let size = u32_at(bytes, at + 8)
            .ok_or_else(|| malformed(format!("truncated size {index}")))?
            as usize;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| malformed(format!("oversized declaration {index}")))?;
        out.insert(
            u64::from(id),
            bytes
                .get(offset..end)
                .ok_or_else(|| malformed(format!("truncated declaration {index}")))?,
        );
    }
    Ok(out)
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdlib() -> Option<KlibArchive> {
        let root = crate::toolchain::kotlin_native_root()?;
        KlibArchive::open(&root.join("klib/common/stdlib")).ok()
    }

    fn key(package: &str, name: &str, parameters: &[&str]) -> IrDefaultKey {
        IrDefaultKey {
            package: package.to_string(),
            owners: Vec::new(),
            name: name.to_string(),
            parameters: parameters.iter().map(|name| name.to_string()).collect(),
        }
    }

    /// The exact values that stood between a klib-backed compilation and 862 corpus cases.
    ///
    /// Each is checked against what the Kotlin source declares, which is the only oracle that
    /// matters here: `assertEquals(expected: T, actual: T, message: String? = null)`,
    /// `joinToString(separator: CharSequence = ", ", prefix: CharSequence = "", …)`.
    #[test]
    fn the_stdlib_states_what_its_parameters_default_to() {
        let Some(archive) = stdlib() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let defaults = read_defaults(&archive).expect("the stdlib's IR reads");
        // Few, and that is the library rather than the reader: a stdlib declaration with a
        // defaulted parameter is uncommon, and most of the ones a program actually calls are in
        // `kotlin.test` and the collection joiners. 342 declarations carry one at all.
        assert!(
            defaults.len() > 200,
            "the whole library was walked: {}",
            defaults.len()
        );

        // `message: String? = null` — the one that blocks every `assertEquals(a, b)` in the box
        // corpus. A `null` is a distinct protobuf field from a `false`, which the line below pins.
        assert_eq!(
            defaults.get(&key(
                "kotlin.test",
                "assertEquals",
                &["expected", "actual", "message"]
            )),
            Some([None, None, Some(IrConstant::Null)].as_slice())
        );
        // A BOOLEAN default beside a null one, on the same declaration.
        assert_eq!(
            defaults.get(&key(
                "kotlin.test",
                "assertContains",
                &["charSequence", "char", "ignoreCase", "message"],
            )),
            Some(
                [
                    None,
                    None,
                    Some(IrConstant::Boolean(false)),
                    Some(IrConstant::Null),
                ]
                .as_slice()
            )
        );
        // STRING defaults, six of them on one declaration, each a different value — the
        // declaration that reported "selected extension 'joinToString' has no callable
        // realization" rather than compiling.
        assert_eq!(
            defaults.get(&key(
                "kotlin.collections",
                "joinToString",
                &[
                    "separator",
                    "prefix",
                    "postfix",
                    "limit",
                    "truncated",
                    "transform"
                ],
            )),
            Some(
                [
                    Some(IrConstant::String(", ".to_string())),
                    Some(IrConstant::String(String::new())),
                    Some(IrConstant::String(String::new())),
                    Some(IrConstant::Int(-1)),
                    Some(IrConstant::String("...".to_string())),
                    Some(IrConstant::Null),
                ]
                .as_slice()
            ),
            "and an INT default written as a negative number, sign-extended rather than zigzagged"
        );
    }

    /// A key several declarations answer differently is published as unknown.
    ///
    /// Two overloads sharing a name, an arity and every parameter name cannot be told apart from
    /// `linkdata`, so answering with either one's value would be a guess — and a guess here is a
    /// silently wrong argument at a call site, which is the one outcome worse than declining.
    #[test]
    fn a_key_two_declarations_disagree_on_answers_nothing() {
        let mut defaults = IrDefaults::default();
        let key = key("p", "f", &["a"]);
        defaults.record(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(
            defaults.get(&key),
            Some([Some(IrConstant::Int(1))].as_slice())
        );
        // The same answer again is agreement, not a conflict.
        defaults.record(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(
            defaults.get(&key),
            Some([Some(IrConstant::Int(1))].as_slice())
        );
        defaults.record(key.clone(), vec![Some(IrConstant::Int(2))]);
        assert_eq!(defaults.get(&key), None);
        // And it stays unknown: a third declaration agreeing with the first must not resurrect it.
        defaults.record(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(defaults.get(&key), None);
    }

    /// The encoding that is not protobuf's own.
    #[test]
    fn a_name_and_a_type_travel_interleaved() {
        // `BinaryLattice.encode(33, 7)` — the pair `kotlin.test`'s first function carries.
        assert_eq!(morton(1067), (33, 7));
        assert_eq!(morton(0), (0, 0));
        // Interleaving is what keeps a pair of small indices a short varint: 1067 is two bytes.
        assert_eq!(morton(0b11), (1, 1));
    }
}
