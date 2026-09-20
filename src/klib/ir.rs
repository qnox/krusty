//! The bodies half of a klib: `default/ir/`, where a parameter's DEFAULT VALUE and an `inline`
//! declaration's BODY live.
//!
//! `linkdata` describes declarations. It records THAT a parameter has a default and never what the
//! default IS — that is an expression, and expressions live here. A call that omits a defaulted
//! argument therefore cannot be compiled from `linkdata` alone: `assertEquals(expected, actual)`
//! needs to know that the third parameter is `null`, and no amount of reading declarations will
//! say so. `inline` is the same fact twice over: `linkdata` says a declaration is one and never
//! says what it does, so a provider that published the modifier alone would promise an expansion
//! it has no body for.
//!
//! **Scope.** Two shapes, and nothing else. A parameter default is read when it is a CONSTANT, and
//! a body when it is nothing but an invocation of one of the declaration's own function-typed
//! parameters — `let`, `run`, `with`, `apply`, `also`. Anything else is reported as absent rather
//! than approximated: a caller that cannot see a value must decline the call, not guess at it. In
//! the Kotlin standard library the overwhelming majority of defaults are `null`, `false`, `""` and
//! small integers, which is why so little buys so much.
//!
//! **Layout.** Each of its tables is an array with one entry per FILE of the library:
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
//! **Encodings.** Three of them are not protobuf's. A name and a type travel together in one
//! `int64` as a Morton code — the two indices' bits interleaved, so that a pair of small numbers
//! stays a short varint. A `float`/`double` constant is written as its raw bits in a fixed-width
//! field rather than as a protobuf `float`/`double`. And a SYMBOL is its signature's index in the
//! file's own table shifted left past a one-byte kind, which is how a body says both what it
//! invokes (`symbol >> 8` names the declaration) and which of its own parameters it read (two
//! symbols comparing equal). Everything else is ordinary protobuf, including the integer
//! constants: they are `int32`/`int64` rather than `sint32`, so a negative default is written
//! sign-extended to ten bytes and must not be read as a zigzag.

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
pub struct IrDeclarationKey {
    /// The package, dotted (`kotlin.test`), empty for the root package.
    pub package: String,
    /// The enclosing classifiers, outermost first, empty for a top-level declaration.
    pub owners: Vec<String>,
    /// The declaration's own name (`assertEquals`, `<init>`).
    pub name: String,
    /// Whether the declaration is an extension. `kotlin.run` is two declarations with one
    /// parameter named `block` apiece, and this is the only thing that tells them apart.
    pub receiver: bool,
    /// The value parameters' source names, in declaration order.
    pub parameters: Vec<String>,
}

/// The shape of an inline declaration whose body is one invocation of its own lambda.
///
/// `let`, `run`, `with`, `apply` and `also` are this and nothing more. What makes reading them
/// worth the trouble is not the call the body makes but the one a CALLER then doesn't: a spliced
/// `with(x) { return y }` returns from the enclosing function, and a called one cannot.
///
/// Ordinals count the declaration's parameters as a call site pushes them — 0 is the extension
/// receiver when there is one, and the regular parameters follow in declaration order. A
/// declaration with a dispatch receiver or a context parameter carries parameters this module does
/// not count, and publishes no body rather than one with shifted ordinals.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrInlineBody {
    /// The parameter the body invokes.
    pub lambda: usize,
    /// The parameters it hands to that invocation, in order.
    pub arguments: Vec<usize>,
    /// The parameter returned INSTEAD of the invocation's result: `apply` and `also` hand back the
    /// value they were applied to. `None` when the invocation's own result is the declaration's.
    pub result: Option<usize>,
}

/// Every declaration's parameter defaults and inline body, read out of one klib's IR.
#[derive(Default)]
pub struct IrBodies {
    /// `None` marks a key several declarations answered DIFFERENTLY. Two overloads that match on
    /// name, arity and every parameter name are indistinguishable here, so a key they disagree on
    /// is published as unknown — which makes a caller decline the call rather than pass a value
    /// taken from the wrong declaration.
    defaults: HashMap<IrDeclarationKey, Option<Vec<Option<IrConstant>>>>,
    /// The same rule, for the declarations whose body this module reads.
    inline: HashMap<IrDeclarationKey, Option<IrInlineBody>>,
}

impl IrBodies {
    /// The defaults of the declaration this key names, parallel to its parameters, or `None` when
    /// the library does not say — a default that is not a constant, or a key more than one
    /// declaration answers differently.
    pub fn defaults(&self, key: &IrDeclarationKey) -> Option<&[Option<IrConstant>]> {
        self.defaults.get(key)?.as_deref()
    }

    /// The body this key's declaration inlines to, or `None` when the library does not say — a
    /// body this module does not read, or a key more than one declaration answers differently.
    pub fn inline_body(&self, key: &IrDeclarationKey) -> Option<&IrInlineBody> {
        self.inline.get(key)?.as_ref()
    }

    /// How many declarations stated a default, for a caller that wants to say the IR really was
    /// read.
    pub fn defaults_len(&self) -> usize {
        self.defaults.len()
    }

    /// How many stated a body this module reads.
    pub fn inline_len(&self) -> usize {
        self.inline.len()
    }

    fn record_defaults(&mut self, key: IrDeclarationKey, values: Vec<Option<IrConstant>>) {
        match self.defaults.get(&key) {
            None => {
                self.defaults.insert(key, Some(values));
            }
            Some(Some(existing)) if *existing == values => {}
            Some(_) => {
                self.defaults.insert(key, None);
            }
        }
    }

    fn record_inline(&mut self, key: IrDeclarationKey, body: IrInlineBody) {
        match self.inline.get(&key) {
            None => {
                self.inline.insert(key, Some(body));
            }
            Some(Some(existing)) if *existing == body => {}
            Some(_) => {
                self.inline.insert(key, None);
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

/// Read every parameter default this klib's IR states as a constant, and every inline body it
/// states as an invocation of the declaration's own lambda.
pub fn read(archive: &KlibArchive) -> Result<IrBodies, IrError> {
    let files = per_file(archive, "files.knf")?;
    let strings = per_file(archive, "strings.knt")?;
    let signatures = per_file(archive, "signatures.knt")?;
    let declarations = per_file(archive, "irDeclarations.knd")?;
    let bodies = per_file(archive, "bodies.knb")?;
    if strings.len() != files.len()
        || signatures.len() != files.len()
        || declarations.len() != files.len()
        || bodies.len() != files.len()
    {
        return Err(IrError::Malformed {
            entry: "ir".to_string(),
            detail: format!(
                "the per-file tables disagree on how many files there are: \
                 {} files, {} string tables, {} signature tables, {} declaration tables, \
                 {} body tables",
                files.len(),
                strings.len(),
                signatures.len(),
                declarations.len(),
                bodies.len()
            ),
        });
    }
    let mut read = IrBodies::default();
    for index in 0..files.len() {
        let strings = entries(&strings[index], "strings.knt")?
            .into_iter()
            .map(|entry| String::from_utf8_lossy(entry).into_owned())
            .collect::<Vec<_>>();
        let signatures = entries(&signatures[index], "signatures.knt")?;
        let bodies = entries(&bodies[index], "bodies.knb")?;
        let declarations = declaration_index(&declarations[index])?;
        let file = File {
            strings: &strings,
            signatures: &signatures,
            bodies: &bodies,
        };
        // `IrFile.fq_name` is field 3: the package's segments as string ids.
        let package = file.qualified(&packed(&files[index], 3))?;
        // `IrFile.declaration_id` is field 1: the file's top-level declarations, by ID.
        for id in packed(&files[index], 1) {
            let Some(body) = declarations.get(&id) else {
                continue;
            };
            walk(&file, body, &package, &mut Vec::new(), &mut read)?;
        }
    }
    Ok(read)
}

/// One file's own tables. Every index a declaration carries is into THESE, not into the library's.
struct File<'a> {
    strings: &'a [String],
    signatures: &'a [&'a [u8]],
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

    /// The arity of the `kotlin.FunctionN.invoke` a call names, and `None` for a call to anything
    /// else. THE way a klib spells "this declaration invokes its lambda": Kotlin's function types
    /// are ordinary declarations, and calling one through a parameter is a call to `invoke`.
    fn invoked_function_arity(&self, symbol: u64) -> Option<usize> {
        let signature = self.signatures.get((symbol >> 8) as usize)?;
        // `IdSignature.public_sig`: a `CommonIdSignature`, whose package and declaration names are
        // both packed runs of string ids. A private or local signature names nothing global.
        let Value::Bytes(public) = first(signature, 1)? else {
            return None;
        };
        (self.qualified(&packed(public, 1)).ok()? == "kotlin").then_some(())?;
        let declaration = self.qualified(&packed(public, 2)).ok()?;
        let arity = declaration
            .strip_suffix(".invoke")?
            .strip_prefix("Function")?;
        (!arity.is_empty() && arity.bytes().all(|digit| digit.is_ascii_digit())).then_some(())?;
        arity.parse().ok()
    }
}

/// Walk one declaration, recording what it and anything nested inside it default to.
fn walk(
    file: &File<'_>,
    declaration: &[u8],
    package: &str,
    owners: &mut Vec<String>,
    into: &mut IrBodies,
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
            // Only a FUNCTION's body is read. A constructor and a property accessor can share a
            // package, name and parameter list with a function they are not, and the key cannot
            // tell them apart — so the one fact whose wrong answer is a miscompiled call site is
            // published only where that collision cannot happen.
            3 | 6 => {
                if let Some(Value::Bytes(base)) = first(body, 1) {
                    function(file, base, package, owners, field == 6, into)?;
                }
            }
            // `ir_property`: its accessors are `IrFunction`s of their own.
            7 => {
                for (accessor, accessor_wire, value) in message(body) {
                    if matches!(accessor, 4 | 5) && accessor_wire == 2 {
                        if let Value::Bytes(function_body) = value {
                            if let Some(Value::Bytes(base)) = first(function_body, 1) {
                                function(file, base, package, owners, false, into)?;
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

/// Record one `IrFunctionBase`'s parameter defaults, and its body when it is one this reads.
fn function(
    file: &File<'_>,
    base: &[u8],
    package: &str,
    owners: &[String],
    reads_body: bool,
    into: &mut IrBodies,
) -> Result<(), IrError> {
    let Some(Value::Varint(name_type)) = first(base, 2) else {
        return Ok(());
    };
    let (name, _) = morton(name_type);
    let mut parameters = Vec::new();
    let mut values = Vec::new();
    let mut defaulted = false;
    // `IrFunctionBase.extension_receiver` is the declaration's first parameter, exactly as a call
    // site pushes it. A dispatch receiver (field 4) and a context parameter (field 9) are
    // parameters this module does not count, so a declaration carrying either publishes no body
    // rather than one whose ordinals are shifted against what a caller will hand it.
    let receiver = first(base, 5).is_some();
    let counted = first(base, 4).is_none() && first(base, 9).is_none();
    let mut symbols = declaration_symbol_of(first(base, 5))
        .into_iter()
        .collect::<Vec<_>>();
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
        symbols.extend(declaration_symbol(parameter));
        let constant = match first(parameter, 4) {
            Some(Value::Varint(body)) => {
                defaulted = true;
                file.bodies
                    .get(body as usize)
                    .and_then(|body| constant(file, body))
            }
            _ => None,
        };
        values.push(constant);
    }
    // Every parameter must have answered with a symbol: a body names the ones it reads, and one
    // this module could not name is one it cannot claim the body left alone.
    let named = symbols.len() == usize::from(receiver) + parameters.len();
    let key = IrDeclarationKey {
        package: package.to_string(),
        owners: owners.to_vec(),
        name: file.name(name as usize)?.to_string(),
        receiver,
        parameters,
    };
    // A declaration with no defaulted parameter at all has nothing to say; recording it would only
    // grow the table with keys no caller can ask about.
    if defaulted {
        into.record_defaults(key.clone(), values);
    }
    if reads_body && counted && named {
        if let Some(body) = inline_body(file, base, &symbols) {
            into.record_inline(key, body);
        }
    }
    Ok(())
}

/// One declaration's own symbol, which is how a body says it read THAT declaration.
///
/// `IrFunctionBase` and `IrValueParameter` both open with an `IrDeclarationBase`, whose first
/// field it is, so the same read answers for a function and for one of its parameters.
fn declaration_symbol(declaration: &[u8]) -> Option<u64> {
    let Value::Bytes(base) = first(declaration, 1)? else {
        return None;
    };
    first_varint(base, 1)
}

fn declaration_symbol_of(declaration: Option<Value<'_>>) -> Option<u64> {
    match declaration? {
        Value::Bytes(bytes) => declaration_symbol(bytes),
        _ => None,
    }
}

/// The body this declaration inlines to, when it is nothing but an invocation of its own lambda.
fn inline_body(file: &File<'_>, base: &[u8], parameters: &[u64]) -> Option<IrInlineBody> {
    let declaration = declaration_symbol(base)?;
    // `IrFunctionBase.body` is an index into this file's own body table.
    let body = file.bodies.get(first_varint(base, 7)? as usize)?;
    // A function's body is an `IrStatement` carrying an `IrBlockBody` — even one written `= expr`,
    // which the IR has already turned into a `return`. A parameter default is an
    // `IrExpressionBody`, a bare `IrExpression`: the same table, two shapes.
    let Value::Bytes(block) = first(body, 4)? else {
        return None;
    };
    let statements = message(block)
        .filter(|(field, wire, _)| *field == 1 && *wire == 2)
        .filter_map(|(_, _, value)| match value {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        })
        .collect::<Vec<_>>();
    match statements.as_slice() {
        // `= block(this)`: the invocation IS what the declaration hands back.
        [single] => {
            let (lambda, arguments) = invocation(file, returned(single, declaration)?, parameters)?;
            Some(IrInlineBody {
                lambda,
                arguments,
                result: None,
            })
        }
        // `{ block(); return this }`: the declaration hands back one of its own parameters.
        [invoke, tail] => {
            let (lambda, arguments) = invocation(file, statement_expression(invoke)?, parameters)?;
            let result = ordinal(parameters, get_value(returned(tail, declaration)?)?)?;
            Some(IrInlineBody {
                lambda,
                arguments,
                result: Some(result),
            })
        }
        _ => None,
    }
}

/// The expression one statement is, and `None` for a statement that is a declaration or a branch.
fn statement_expression(statement: &[u8]) -> Option<&[u8]> {
    match first(statement, 3)? {
        Value::Bytes(bytes) => Some(bytes),
        _ => None,
    }
}

/// The value a `return` to THIS declaration hands back. A return whose target is anything else —
/// a returnable block, an enclosing lambda — is control flow this module does not read.
fn returned(statement: &[u8], declaration: u64) -> Option<&[u8]> {
    let (operation, value) = operation(statement_expression(statement)?)?;
    (operation == 12).then_some(())?;
    (first_varint(value, 1)? == declaration).then_some(())?;
    match first(value, 2)? {
        Value::Bytes(bytes) => Some(bytes),
        _ => None,
    }
}

/// The symbol of the value an expression reads, and `None` for an expression that reads none.
fn get_value(expression: &[u8]) -> Option<u64> {
    let (operation, value) = operation(expression)?;
    (operation == 6).then_some(())?;
    first_varint(value, 1)
}

fn ordinal(parameters: &[u64], symbol: u64) -> Option<usize> {
    parameters.iter().position(|parameter| *parameter == symbol)
}

/// One `FunctionN.invoke` whose every argument reads one of this declaration's own parameters, as
/// the parameter invoked and the ones handed to it.
///
/// The arity check is what keeps the shape honest: `invoke`'s first argument is the function
/// value itself, so a call naming `Function1.invoke` must carry exactly two.
fn invocation(
    file: &File<'_>,
    expression: &[u8],
    parameters: &[u64],
) -> Option<(usize, Vec<usize>)> {
    let (operation, call) = operation(expression)?;
    (operation == 8).then_some(())?;
    // `IrCall.super`: a qualified dispatch, which an invocation of a parameter never is.
    first(call, 3).is_none().then_some(())?;
    let arity = file.invoked_function_arity(first_varint(call, 1)?)?;
    let mut read = Vec::new();
    for (field, wire, value) in message(call) {
        if field != 5 || wire != 2 {
            continue;
        }
        let Value::Bytes(argument) = value else {
            return None;
        };
        read.push(ordinal(parameters, get_value(argument)?)?);
    }
    let (lambda, arguments) = read.split_first()?;
    (arguments.len() == arity).then_some(())?;
    Some((*lambda, arguments.to_vec()))
}

/// The single operation an `IrExpression` carries, as its field number and payload.
///
/// The field is a protobuf `oneof`, so a message carrying two is one no Kotlin toolchain wrote and
/// none this module reads.
fn operation(expression: &[u8]) -> Option<(u64, &[u8])> {
    let mut found = None;
    for (field, wire, value) in message(expression) {
        if wire != 2 || !(5..=44).contains(&field) {
            continue;
        }
        let Value::Bytes(bytes) = value else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some((field, bytes));
    }
    found
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

    fn key(package: &str, name: &str, parameters: &[&str]) -> IrDeclarationKey {
        IrDeclarationKey {
            package: package.to_string(),
            owners: Vec::new(),
            name: name.to_string(),
            receiver: false,
            parameters: parameters.iter().map(|name| name.to_string()).collect(),
        }
    }

    fn extension(package: &str, name: &str, parameters: &[&str]) -> IrDeclarationKey {
        IrDeclarationKey {
            receiver: true,
            ..key(package, name, parameters)
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
        let defaults = read(&archive).expect("the stdlib's IR reads");
        // Few, and that is the library rather than the reader: a stdlib declaration with a
        // defaulted parameter is uncommon, and most of the ones a program actually calls are in
        // `kotlin.test` and the collection joiners. 342 declarations carry one at all.
        assert!(
            defaults.defaults_len() > 200,
            "the whole library was walked: {}",
            defaults.defaults_len()
        );

        // `message: String? = null` — the one that blocks every `assertEquals(a, b)` in the box
        // corpus. A `null` is a distinct protobuf field from a `false`, which the line below pins.
        assert_eq!(
            defaults.defaults(&key(
                "kotlin.test",
                "assertEquals",
                &["expected", "actual", "message"]
            )),
            Some([None, None, Some(IrConstant::Null)].as_slice())
        );
        // A BOOLEAN default beside a null one, on the same declaration.
        assert_eq!(
            defaults.defaults(&key(
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
            defaults.defaults(&extension(
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
        let mut defaults = IrBodies::default();
        let key = key("p", "f", &["a"]);
        defaults.record_defaults(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(
            defaults.defaults(&key),
            Some([Some(IrConstant::Int(1))].as_slice())
        );
        // The same answer again is agreement, not a conflict.
        defaults.record_defaults(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(
            defaults.defaults(&key),
            Some([Some(IrConstant::Int(1))].as_slice())
        );
        defaults.record_defaults(key.clone(), vec![Some(IrConstant::Int(2))]);
        assert_eq!(defaults.defaults(&key), None);
        // And it stays unknown: a third declaration agreeing with the first must not resurrect it.
        defaults.record_defaults(key.clone(), vec![Some(IrConstant::Int(1))]);
        assert_eq!(defaults.defaults(&key), None);
    }

    /// The same rule for bodies, which is why an extension is part of the key at all.
    ///
    /// `kotlin.run` is two declarations — `run(block: () -> R)` and `T.run(block: T.() -> R)` —
    /// with one parameter named `block` apiece. Without the receiver in the key they collide, and
    /// what collides here is not a value a caller can decline but a SPLICE: the receiver-less one
    /// would expand with an argument it has nowhere to take from.
    #[test]
    fn a_body_two_declarations_disagree_on_answers_nothing() {
        let mut bodies = IrBodies::default();
        let plain = IrInlineBody {
            lambda: 0,
            arguments: Vec::new(),
            result: None,
        };
        let extended = IrInlineBody {
            lambda: 1,
            arguments: vec![0],
            result: None,
        };
        bodies.record_inline(key("p", "f", &["a"]), plain.clone());
        bodies.record_inline(extension("p", "f", &["a"]), extended.clone());
        assert_eq!(bodies.inline_body(&key("p", "f", &["a"])), Some(&plain));
        assert_eq!(
            bodies.inline_body(&extension("p", "f", &["a"])),
            Some(&extended)
        );
        // And two that really do share a key publish nothing rather than either one's body.
        bodies.record_inline(key("p", "f", &["a"]), extended);
        assert_eq!(bodies.inline_body(&key("p", "f", &["a"])), None);
    }

    /// The scope functions, read out of the real stdlib.
    ///
    /// Each is checked against what the Kotlin source declares, which is the only oracle that
    /// matters: `T.let(block: (T) -> R): R = block(this)` invokes its second parameter with its
    /// first, `T.apply(block: T.() -> Unit): T` does the same and then hands back the first, and
    /// `run(block: () -> R): R = block()` invokes its only one with nothing.
    #[test]
    fn the_stdlib_states_what_its_scope_functions_do() {
        let Some(archive) = stdlib() else {
            eprintln!("skipping: no Kotlin/Native distribution cached");
            return;
        };
        let bodies = read(&archive).expect("the stdlib's IR reads");
        assert!(
            bodies.inline_len() > 0,
            "the whole library was walked for bodies"
        );
        let invoking = |lambda: usize, arguments: &[usize]| {
            Some(IrInlineBody {
                lambda,
                arguments: arguments.to_vec(),
                result: None,
            })
        };
        // `T.let`, `T.run` and `with` all invoke one lambda with one value — the difference is
        // only where that value came from, and the ordinals say so identically.
        assert_eq!(
            bodies
                .inline_body(&extension("kotlin", "let", &["block"]))
                .cloned(),
            invoking(1, &[0])
        );
        assert_eq!(
            bodies
                .inline_body(&extension("kotlin", "run", &["block"]))
                .cloned(),
            invoking(1, &[0])
        );
        assert_eq!(
            bodies
                .inline_body(&key("kotlin", "with", &["receiver", "block"]))
                .cloned(),
            invoking(1, &[0])
        );
        // The receiver-less `run`, which the extension bit is the only thing separating.
        assert_eq!(
            bodies
                .inline_body(&key("kotlin", "run", &["block"]))
                .cloned(),
            invoking(0, &[])
        );
        // `apply` and `also` hand back what they were applied to rather than what the lambda
        // returned. Reading that wrong is a silently wrong VALUE at every call site.
        for name in ["apply", "also"] {
            assert_eq!(
                bodies
                    .inline_body(&extension("kotlin", name, &["block"]))
                    .cloned(),
                Some(IrInlineBody {
                    lambda: 1,
                    arguments: vec![0],
                    result: Some(0),
                }),
                "kotlin.{name}"
            );
        }
        // And a declaration whose body is not one invocation states nothing: `takeIf` tests its
        // lambda's result, and approximating that would drop the test.
        assert_eq!(
            bodies.inline_body(&extension("kotlin", "takeIf", &["predicate"])),
            None
        );
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
