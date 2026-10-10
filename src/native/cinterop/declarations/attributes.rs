//! GNU attributes (`__attribute__((…))`), `_Alignas` and `__asm__("name")` labels: the parts of a
//! declaration that change a layout or a symbol rather than a type's shape.
//!
//! Only what a binding depends on is kept — `aligned`, `packed`, `mode`, `vector_size` and
//! `noreturn`. Every other attribute (`__nothrow__`, `__nonnull__ (1)`, `__format__ (…)`, …)
//! describes code cinterop never compiles, and is read past.

use std::rc::Rc;

use super::super::layout::DEFAULT_ATTRIBUTE_ALIGNMENT;
use super::super::lexer::Kind;
use super::super::model::{IntRank, Signedness, Type, TypeId};
use super::parser::Parser;
use super::ParseError;

/// The attributes one declaration, declarator or record carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Attributes {
    /// In bytes; the largest when several are given, as GCC takes it.
    pub(super) aligned: Option<u64>,
    pub(super) packed: bool,
    /// `__mode__ (…)`: the integer type to use in place of the declared one.
    pub(super) mode: Option<IntRank>,
    /// `vector_size (n)`: the declared type is the element of an `n`-byte vector.
    pub(super) vector_size: Option<u64>,
    pub(super) noreturn: bool,
}

impl Attributes {
    pub(super) fn merge(&mut self, other: &Attributes) {
        self.aligned = self.aligned.max(other.aligned);
        self.packed |= other.packed;
        self.mode = other.mode.or(self.mode);
        self.vector_size = other.vector_size.or(self.vector_size);
        self.noreturn |= other.noreturn;
    }
}

impl Parser<'_> {
    pub(super) fn at_attribute(&self) -> bool {
        self.at_any(&["__attribute__", "__attribute"])
    }

    /// Read every attribute specifier at the cursor into `into`.
    pub(super) fn attributes(&mut self, into: &mut Attributes) -> Result<(), ParseError> {
        while self.at_attribute() {
            self.next();
            self.expect("(")?;
            self.expect("(")?;
            loop {
                if self.eat(",") {
                    continue;
                }
                if self.at(")") {
                    break;
                }
                self.attribute(into)?;
            }
            self.expect(")")?;
            self.expect(")")?;
        }
        Ok(())
    }

    fn attribute(&mut self, into: &mut Attributes) -> Result<(), ParseError> {
        let name = self.peek().filter(|token| token.kind == Kind::Identifier);
        let Some(name) = name else {
            return Err(self.error(&format!("expected an attribute name{}", self.found())));
        };
        self.next();
        let bare = name.text.trim_start_matches("__").trim_end_matches("__");
        match bare {
            "aligned" => {
                let alignment = if self.eat("(") {
                    let value = self.constant_expression()?;
                    self.expect(")")?;
                    self.alignment_value(value.as_u64())?
                } else {
                    DEFAULT_ATTRIBUTE_ALIGNMENT
                };
                into.aligned = into.aligned.max(Some(alignment));
            }
            "packed" => into.packed = true,
            "noreturn" => into.noreturn = true,
            "mode" => {
                self.expect("(")?;
                let mode = self.identifier()?;
                let rank = match mode.text.trim_start_matches("__").trim_end_matches("__") {
                    "QI" | "byte" => IntRank::Char,
                    "HI" => IntRank::Short,
                    "SI" => IntRank::Int,
                    "DI" | "word" | "pointer" => IntRank::Long,
                    "TI" => IntRank::Int128,
                    other => {
                        return Err(self.error(&format!("unsupported mode '{other}'")));
                    }
                };
                self.expect(")")?;
                into.mode = Some(rank);
            }
            "vector_size" => {
                self.expect("(")?;
                let size = self.constant_expression()?;
                self.expect(")")?;
                into.vector_size = Some(size.as_u64());
            }
            _ => {
                if self.at("(") {
                    self.skip_group()?;
                }
            }
        }
        Ok(())
    }

    /// `_Alignas ( type-name )` or `_Alignas ( constant-expression )`, in bytes; the keyword is
    /// at the cursor.
    pub(super) fn alignas(&mut self) -> Result<u64, ParseError> {
        self.next();
        self.expect("(")?;
        let origin = self.origin();
        let alignment = if self.starts_type_name() {
            let ty = self.type_name()?;
            self.layouts
                .layout(&self.declarations, ty)
                .map_err(|error| self.layout_error(&origin, error))?
                .align
        } else {
            let value = self.constant_expression()?;
            self.alignment_value(value.as_u64())?
        };
        self.expect(")")?;
        Ok(alignment)
    }

    fn alignment_value(&self, value: u64) -> Result<u64, ParseError> {
        if value.is_power_of_two() {
            Ok(value)
        } else {
            Err(self.error(&format!("requested alignment {value} is not a power of 2")))
        }
    }

    /// An `__asm__ ("name")` label, when one is at the cursor: the symbol the declaration binds
    /// to. glibc spells it `__asm__ ("" "name")`, so adjacent strings are joined.
    pub(super) fn asm_label(&mut self) -> Result<Option<Rc<str>>, ParseError> {
        if !self.at_any(&["__asm__", "__asm", "asm"]) {
            return Ok(None);
        }
        self.next();
        self.expect("(")?;
        let mut label = String::new();
        while let Some(token) = self
            .peek()
            .filter(|token| token.kind == Kind::StringLiteral)
        {
            let text = &token.text;
            if !text.starts_with('"') || text.contains('\\') {
                return Err(self.error(&format!("unsupported asm label {text}")));
            }
            label.push_str(&text[1..text.len() - 1]);
            self.next();
        }
        self.expect(")")?;
        Ok(Some(Rc::from(label)))
    }

    /// `ty` as the declaration's `mode` and `vector_size` attributes make it.
    pub(super) fn apply_type_attributes(
        &mut self,
        ty: TypeId,
        attributes: &Attributes,
    ) -> Result<TypeId, ParseError> {
        let mut ty = ty;
        if let Some(rank) = attributes.mode {
            let canonical = self.declarations.canonical(ty);
            let Type::Int { signedness, .. } = *self.declarations.ty(canonical) else {
                return Err(self.error("the mode attribute applies only to an integer type"));
            };
            let signedness = match signedness {
                Signedness::Unsigned => Signedness::Unsigned,
                Signedness::Signed | Signedness::Plain => Signedness::Signed,
            };
            ty = self.int_type(rank, signedness);
        }
        if let Some(size) = attributes.vector_size {
            ty = self.declarations.intern(Type::Vector { element: ty, size });
        }
        Ok(ty)
    }
}
