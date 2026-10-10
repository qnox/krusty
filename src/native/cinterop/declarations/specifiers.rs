//! Declaration specifiers (C11 §6.7.1–6.7.5): storage class, type specifiers and qualifiers,
//! function specifiers and alignment, and the struct, union and enum definitions a specifier list
//! may contain.

use std::rc::Rc;

use super::super::lexer::{Kind, Token};
use super::super::model::{
    Enum, Enumerator, Field, FloatKind, IntRank, Qualifiers, Record, RecordKind, Signedness, Type,
    TypeId,
};
use super::attributes::Attributes;
use super::constant::Value;
use super::declarators::DeclaratorKind;
use super::parser::{Ordinary, Parser, Tag};
use super::ParseError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StorageClass {
    Typedef,
    Extern,
    Static,
    Auto,
    Register,
}

/// A declaration's specifiers, resolved.
#[derive(Clone, Debug)]
pub(super) struct Specifiers {
    pub(super) storage: Option<StorageClass>,
    pub(super) thread_local: bool,
    pub(super) inline: bool,
    pub(super) noreturn: bool,
    /// The specified type, with its qualifiers.
    pub(super) ty: TypeId,
    /// Attributes and `_Alignas` written among the specifiers, which apply to each declarator.
    pub(super) attributes: Attributes,
    /// Whether the specifiers define a struct or union without a tag: such a member with no
    /// declarator is an anonymous member (C11 §6.7.2.1p13).
    pub(super) anonymous_record: bool,
}

/// Every keyword that can begin a list of declaration specifiers.
const SPECIFIER_KEYWORDS: &[&str] = &[
    "typedef",
    "extern",
    "static",
    "auto",
    "register",
    "_Thread_local",
    "__thread",
    "inline",
    "__inline",
    "__inline__",
    "_Noreturn",
    "const",
    "__const",
    "__const__",
    "volatile",
    "__volatile",
    "__volatile__",
    "restrict",
    "__restrict",
    "__restrict__",
    "_Atomic",
    "__extension__",
    "__attribute__",
    "__attribute",
    "_Alignas",
    "void",
    "_Bool",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "signed",
    "__signed",
    "__signed__",
    "unsigned",
    "_Complex",
    "__complex__",
    "__int128",
    "struct",
    "union",
    "enum",
    "__typeof__",
    "__typeof",
    "typeof",
];

/// The basic type specifiers seen, which combine into one type (C11 §6.7.2p2).
#[derive(Default)]
struct BasicSpecifiers {
    void: bool,
    bool_: bool,
    char_: bool,
    short: bool,
    int: bool,
    long: u8,
    float: bool,
    double: bool,
    signed: bool,
    unsigned: bool,
    complex: bool,
    int128: bool,
}

impl BasicSpecifiers {
    fn any(&self) -> bool {
        self.void
            || self.bool_
            || self.char_
            || self.short
            || self.int
            || self.long > 0
            || self.float
            || self.double
            || self.signed
            || self.unsigned
            || self.complex
            || self.int128
    }
}

impl Parser<'_> {
    /// Whether `token` can begin declaration specifiers (or a type name).
    pub(super) fn begins_specifiers(&self, token: &Token) -> bool {
        token.kind == Kind::Identifier
            && (SPECIFIER_KEYWORDS.contains(&&*token.text)
                || self.typedef_named(&token.text).is_some())
    }

    pub(super) fn starts_type_name(&self) -> bool {
        self.peek()
            .is_some_and(|token| self.begins_specifiers(token))
    }

    /// Declaration specifiers; a storage class is allowed only where `storage_allowed`.
    pub(super) fn specifiers(&mut self, storage_allowed: bool) -> Result<Specifiers, ParseError> {
        let start = self.origin();
        let mut storage = None;
        let mut thread_local = false;
        let mut inline = false;
        let mut noreturn = false;
        let mut qualifiers = Qualifiers::default();
        let mut atomic = false;
        let mut attributes = Attributes::default();
        let mut basic = BasicSpecifiers::default();
        let mut explicit: Option<TypeId> = None;
        let mut anonymous_record = false;
        while let Some(token) = self.peek().filter(|token| token.kind == Kind::Identifier) {
            let text = &*token.text;
            let mut set_storage = |class| {
                if storage.replace(class).is_some() {
                    Err("multiple storage classes in declaration specifiers")
                } else {
                    Ok(())
                }
            };
            let storage_result = match text {
                "typedef" => Some(set_storage(StorageClass::Typedef)),
                "extern" => Some(set_storage(StorageClass::Extern)),
                "static" => Some(set_storage(StorageClass::Static)),
                "auto" => Some(set_storage(StorageClass::Auto)),
                "register" => Some(set_storage(StorageClass::Register)),
                _ => None,
            };
            if let Some(result) = storage_result {
                result.map_err(|message| self.error(message))?;
                if !storage_allowed {
                    return Err(self.error(&format!("'{text}' is not allowed here")));
                }
                self.next();
                continue;
            }
            match text {
                "_Thread_local" | "__thread" => thread_local = true,
                "inline" | "__inline" | "__inline__" => inline = true,
                "_Noreturn" => noreturn = true,
                "const" | "__const" | "__const__" => {
                    qualifiers = qualifiers.union(Qualifiers::CONST);
                }
                "volatile" | "__volatile" | "__volatile__" => {
                    qualifiers = qualifiers.union(Qualifiers::VOLATILE);
                }
                "restrict" | "__restrict" | "__restrict__" => {
                    qualifiers = qualifiers.union(Qualifiers::RESTRICT);
                }
                "__extension__" => {}
                "_Atomic" if self.peek_at(1).is_some_and(|next| next.is("(")) => {
                    self.next();
                    self.next();
                    let value = self.type_name()?;
                    self.expect(")")?;
                    let ty = self.declarations.intern(Type::Atomic(value));
                    self.set_explicit(&mut explicit, &basic, ty)?;
                    continue;
                }
                "_Atomic" => atomic = true,
                "__attribute__" | "__attribute" => {
                    self.attributes(&mut attributes)?;
                    continue;
                }
                "_Alignas" => {
                    let alignment = self.alignas()?;
                    attributes.aligned = attributes.aligned.max(Some(alignment));
                    continue;
                }
                "struct" | "union" => {
                    let kind = if text == "struct" {
                        RecordKind::Struct
                    } else {
                        RecordKind::Union
                    };
                    let (ty, anonymous) = self.record_specifier(kind)?;
                    anonymous_record = anonymous;
                    self.set_explicit(&mut explicit, &basic, ty)?;
                    continue;
                }
                "enum" => {
                    let ty = self.enum_specifier()?;
                    self.set_explicit(&mut explicit, &basic, ty)?;
                    continue;
                }
                "__typeof__" | "__typeof" | "typeof" => {
                    self.next();
                    self.expect("(")?;
                    let ty = if self.starts_type_name() {
                        self.type_name()?
                    } else {
                        self.expression_type()?
                    };
                    self.expect(")")?;
                    self.set_explicit(&mut explicit, &basic, ty)?;
                    continue;
                }
                "void" => basic.void = true,
                "_Bool" => basic.bool_ = true,
                "char" => basic.char_ = true,
                "short" => basic.short = true,
                "int" => basic.int = true,
                "long" => basic.long += 1,
                "float" => basic.float = true,
                "double" => basic.double = true,
                "signed" | "__signed" | "__signed__" => basic.signed = true,
                "unsigned" => basic.unsigned = true,
                "_Complex" | "__complex__" => basic.complex = true,
                "__int128" => basic.int128 = true,
                _ => {
                    // A typedef name is a type specifier only where no other one was given yet;
                    // after one, the same name is what the declarator declares.
                    match self.typedef_named(text) {
                        Some(typedef) if explicit.is_none() && !basic.any() => {
                            let ty = self.declarations.intern(Type::Typedef(typedef));
                            explicit = Some(ty);
                        }
                        _ => break,
                    }
                }
            }
            if explicit.is_some() && basic.any() {
                return Err(self.error("cannot combine with previous declaration specifier"));
            }
            self.next();
        }
        let base = match explicit {
            Some(ty) => ty,
            None => self.basic_type(&basic, &start)?,
        };
        let base = if atomic {
            self.declarations.intern(Type::Atomic(base))
        } else {
            base
        };
        let ty = if qualifiers.is_empty() {
            base
        } else {
            self.declarations
                .intern(Type::Qualified { base, qualifiers })
        };
        Ok(Specifiers {
            storage,
            thread_local,
            inline,
            noreturn,
            ty,
            attributes,
            anonymous_record,
        })
    }

    fn set_explicit(
        &self,
        explicit: &mut Option<TypeId>,
        basic: &BasicSpecifiers,
        ty: TypeId,
    ) -> Result<(), ParseError> {
        if explicit.is_some() || basic.any() {
            return Err(self.error("cannot combine with previous declaration specifier"));
        }
        *explicit = Some(ty);
        Ok(())
    }

    /// The type the basic specifiers combine to.
    fn basic_type(
        &mut self,
        basic: &BasicSpecifiers,
        origin: &super::super::model::Origin,
    ) -> Result<TypeId, ParseError> {
        if !basic.any() {
            return Err(self.error_at(origin, &format!("type specifier missing{}", self.found())));
        }
        let signedness = if basic.unsigned {
            Signedness::Unsigned
        } else {
            Signedness::Signed
        };
        if basic.signed && basic.unsigned {
            return Err(self.error_at(origin, "both 'signed' and 'unsigned' in declaration"));
        }
        let float = if basic.float {
            Some(FloatKind::Float)
        } else if basic.double && basic.long == 1 {
            Some(FloatKind::LongDouble)
        } else if basic.double {
            Some(FloatKind::Double)
        } else if basic.complex && !(basic.int || basic.char_ || basic.short || basic.long > 0) {
            // A bare `_Complex` is GNU's `_Complex double`.
            Some(FloatKind::Double)
        } else {
            None
        };
        let ty = if basic.void {
            Type::Void
        } else if basic.bool_ {
            Type::Int {
                rank: IntRank::Bool,
                signedness: Signedness::Unsigned,
            }
        } else if let Some(kind) = float {
            if basic.complex {
                Type::Complex(kind)
            } else {
                Type::Float(kind)
            }
        } else if basic.complex {
            return Err(self.error_at(origin, "complex integer types are not supported"));
        } else {
            let rank = if basic.char_ {
                IntRank::Char
            } else if basic.int128 {
                IntRank::Int128
            } else if basic.short {
                IntRank::Short
            } else if basic.long == 1 {
                IntRank::Long
            } else if basic.long == 2 {
                IntRank::LongLong
            } else if basic.long > 2 {
                return Err(self.error_at(origin, "'long long long' is too long"));
            } else {
                IntRank::Int
            };
            let signedness = if rank == IntRank::Char && !basic.signed && !basic.unsigned {
                Signedness::Plain
            } else {
                signedness
            };
            Type::Int { rank, signedness }
        };
        Ok(self.declarations.intern(ty))
    }

    /// The tag a specifier names, when it already names one of `kind`.
    fn existing_tag(&self, tag: &str, wanted: TagKind) -> Result<Option<Tag>, ParseError> {
        let Some(&existing) = self.tags.get(tag) else {
            return Ok(None);
        };
        let matches = match (existing, wanted) {
            (Tag::Record(id), TagKind::Record(kind)) => self.declarations.record(id).kind == kind,
            (Tag::Enum(_), TagKind::Enum) => true,
            _ => false,
        };
        if !matches {
            return Err(self.error(&format!(
                "use of '{tag}' with tag type that does not match previous declaration"
            )));
        }
        Ok(Some(existing))
    }

    /// `struct`/`union` [attributes] [tag] [`{` members `}` [attributes]]. Returns the type and
    /// whether it defined a record without a tag.
    fn record_specifier(&mut self, kind: RecordKind) -> Result<(TypeId, bool), ParseError> {
        // A record is where its keyword is, as C compilers report it.
        let origin = self.origin();
        self.next();
        let mut attributes = Attributes::default();
        self.attributes(&mut attributes)?;
        let tag = if self
            .peek()
            .is_some_and(|token| token.kind == Kind::Identifier)
        {
            Some(self.next().expect("peeked").text.clone())
        } else {
            None
        };
        let existing = match &tag {
            Some(tag) => self.existing_tag(tag, TagKind::Record(kind))?,
            None => None,
        };
        let id = match existing {
            Some(Tag::Record(id)) => id,
            _ => {
                if tag.is_none() && !self.at("{") {
                    return Err(self.error(&format!("expected identifier or '{{'{}", self.found())));
                }
                let id = self.declarations.add_record(Record {
                    kind,
                    tag: tag.clone(),
                    fields: None,
                    packed: false,
                    aligned: None,
                    origin: origin.clone(),
                });
                if let Some(tag) = &tag {
                    self.tags.insert(tag.clone(), Tag::Record(id));
                }
                id
            }
        };
        if !self.eat("{") {
            return Ok((self.declarations.intern(Type::Record(id)), false));
        }
        if self.declarations.record(id).fields.is_some() {
            return Err(self.error_at(
                &origin,
                &format!("redefinition of '{}'", tag.as_deref().unwrap_or("")),
            ));
        }
        let fields = self.member_declarations()?;
        self.expect("}")?;
        self.attributes(&mut attributes)?;
        let record = self.declarations.record_mut(id);
        record.fields = Some(fields);
        record.packed = attributes.packed;
        record.aligned = attributes.aligned;
        record.origin = origin;
        Ok((self.declarations.intern(Type::Record(id)), tag.is_none()))
    }

    /// The members of a struct or union, up to its closing brace.
    fn member_declarations(&mut self) -> Result<Vec<Field>, ParseError> {
        let mut fields = Vec::new();
        while !self.at("}") {
            if self.peek().is_none() {
                return Err(self.error("expected '}'"));
            }
            if self.eat(";") {
                continue;
            }
            if self.at_any(&["_Static_assert", "static_assert"]) {
                self.static_assertion()?;
                continue;
            }
            let specifiers = self.specifiers(false)?;
            if self.eat(";") {
                if specifiers.anonymous_record {
                    fields.push(Field {
                        name: None,
                        ty: specifiers.ty,
                        bit_width: None,
                        aligned: specifiers.attributes.aligned,
                        packed: specifiers.attributes.packed,
                    });
                }
                continue;
            }
            loop {
                let mut declarator = if self.at(":") {
                    self.empty_declarator()
                } else {
                    self.declarator(DeclaratorKind::Named)?
                };
                let origin = declarator.origin.clone().unwrap_or_else(|| self.origin());
                let ty = self.declared_type(&specifiers, &declarator, &origin)?;
                let bit_width = if self.eat(":") {
                    let width = self.constant_expression()?;
                    if width.is_negative() {
                        return Err(self.error_at(&origin, "bitfield has a negative width"));
                    }
                    Some(width.as_u64())
                } else {
                    None
                };
                self.attributes(&mut declarator.attributes)?;
                let mut attributes = specifiers.attributes.clone();
                attributes.merge(&declarator.attributes);
                if bit_width.is_none() && declarator.name.is_none() {
                    return Err(self.error_at(&origin, "a member must have a name"));
                }
                fields.push(Field {
                    name: declarator.name.clone(),
                    ty,
                    bit_width,
                    aligned: attributes.aligned,
                    packed: attributes.packed,
                });
                if self.eat(",") {
                    continue;
                }
                self.expect(";")?;
                break;
            }
        }
        Ok(fields)
    }

    /// `enum` [attributes] [tag] [`{` enumerators `}` [attributes]].
    fn enum_specifier(&mut self) -> Result<TypeId, ParseError> {
        let origin = self.origin();
        self.next();
        let mut attributes = Attributes::default();
        self.attributes(&mut attributes)?;
        let tag = if self
            .peek()
            .is_some_and(|token| token.kind == Kind::Identifier)
        {
            Some(self.next().expect("peeked").text.clone())
        } else {
            None
        };
        let existing = match &tag {
            Some(tag) => self.existing_tag(tag, TagKind::Enum)?,
            None => None,
        };
        let id = match existing {
            Some(Tag::Enum(id)) => id,
            _ => {
                if tag.is_none() && !self.at("{") {
                    return Err(self.error(&format!("expected identifier or '{{'{}", self.found())));
                }
                let id = self.declarations.add_enum(Enum {
                    tag: tag.clone(),
                    enumerators: None,
                    packed: false,
                    aligned: None,
                    origin: origin.clone(),
                });
                if let Some(tag) = &tag {
                    self.tags.insert(tag.clone(), Tag::Enum(id));
                }
                id
            }
        };
        if !self.eat("{") {
            return Ok(self.declarations.intern(Type::Enum(id)));
        }
        if self.declarations.enumeration(id).enumerators.is_some() {
            return Err(self.error_at(
                &origin,
                &format!("redefinition of 'enum {}'", tag.as_deref().unwrap_or("")),
            ));
        }
        let mut enumerators = Vec::new();
        let mut next_value: i128 = 0;
        while !self.at("}") {
            let name: Rc<str> = self.identifier()?.text.clone();
            let mut ignored = Attributes::default();
            self.attributes(&mut ignored)?;
            let value = if self.eat("=") {
                self.constant_expression()?.value
            } else {
                next_value
            };
            if self.ordinary.contains_key(&name) {
                return Err(self.error(&format!("redefinition of enumerator '{name}'")));
            }
            self.ordinary
                .insert(name.clone(), Ordinary::Constant(Value::enumerator(value)));
            enumerators.push(Enumerator { name, value });
            next_value = value + 1;
            if !self.eat(",") {
                break;
            }
        }
        self.expect("}")?;
        self.attributes(&mut attributes)?;
        let enumeration = self.declarations.enumeration_mut(id);
        enumeration.enumerators = Some(enumerators);
        enumeration.packed = attributes.packed;
        enumeration.aligned = attributes.aligned;
        enumeration.origin = origin;
        Ok(self.declarations.intern(Type::Enum(id)))
    }
}

#[derive(Clone, Copy)]
enum TagKind {
    Record(RecordKind),
    Enum,
}
