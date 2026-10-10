//! The parser's state — the token cursor and C's file scope — and the declarations that make up a
//! translation unit: what a declaration declares once its specifiers and declarators are read.

use std::collections::HashMap;
use std::rc::Rc;

use super::super::layout::{LayoutEngine, LayoutError};
use super::super::lexer::{Kind, Token};
use super::super::model::{
    Declarations, EnumId, Function, IntRank, Origin, RecordId, Signedness, Storage, Type, TypeId,
    Typedef, TypedefId, Variable,
};
use super::constant::Value;
use super::declarators::{Declarator, DeclaratorKind};
use super::specifiers::{Specifiers, StorageClass};
use super::{Header, ParseError};
use crate::native::target::NativeTarget;

/// What a struct, union or enum tag names.
#[derive(Clone, Copy, Debug)]
pub(super) enum Tag {
    Record(RecordId),
    Enum(EnumId),
}

/// What an ordinary identifier names at file scope.
#[derive(Clone, Copy, Debug)]
pub(super) enum Ordinary {
    Typedef(TypedefId),
    /// An enumeration constant.
    Constant(Value),
    /// A function or variable, with its type.
    Object(TypeId),
}

pub(super) struct Parser<'t> {
    tokens: &'t [Token],
    at: usize,
    pub(super) declarations: Declarations,
    pub(super) layouts: LayoutEngine,
    pub(super) tags: HashMap<Rc<str>, Tag>,
    pub(super) ordinary: HashMap<Rc<str>, Ordinary>,
    functions: HashMap<Rc<str>, usize>,
    variables: HashMap<Rc<str>, usize>,
}

impl<'t> Parser<'t> {
    pub(super) fn new(tokens: &'t [Token], target: NativeTarget) -> Self {
        let mut parser = Self {
            tokens,
            at: 0,
            declarations: Declarations::default(),
            layouts: LayoutEngine::new(target),
            tags: HashMap::new(),
            ordinary: HashMap::new(),
            functions: HashMap::new(),
            variables: HashMap::new(),
        };
        // The typedefs a C compiler provides without a header.
        let va_list = parser.declarations.intern(Type::VaList);
        let int128 = parser.int_type(IntRank::Int128, Signedness::Signed);
        let uint128 = parser.int_type(IntRank::Int128, Signedness::Unsigned);
        for (name, ty) in [
            ("__builtin_va_list", va_list),
            ("__int128_t", int128),
            ("__uint128_t", uint128),
        ] {
            let name: Rc<str> = Rc::from(name);
            let id = parser.declarations.add_typedef(Typedef {
                name: name.clone(),
                ty,
                aligned: None,
                origin: None,
            });
            parser.ordinary.insert(name, Ordinary::Typedef(id));
        }
        parser
    }

    pub(super) fn finish(self) -> Header {
        Header {
            declarations: self.declarations,
            layouts: self.layouts,
        }
    }

    // ---- the token cursor ---------------------------------------------------------------------

    pub(super) fn peek(&self) -> Option<&'t Token> {
        self.tokens.get(self.at)
    }

    pub(super) fn peek_at(&self, offset: usize) -> Option<&'t Token> {
        self.tokens.get(self.at + offset)
    }

    pub(super) fn position(&self) -> usize {
        self.at
    }

    /// Return the cursor to a position [`Parser::position`] gave.
    pub(super) fn rewind(&mut self, position: usize) {
        self.at = position;
    }

    pub(super) fn next(&mut self) -> Option<&'t Token> {
        let token = self.tokens.get(self.at);
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    /// Whether the next token is the punctuator or keyword `text`.
    pub(super) fn at(&self, text: &str) -> bool {
        self.peek().is_some_and(|token| token.is(text))
    }

    pub(super) fn at_any(&self, texts: &[&str]) -> bool {
        self.peek()
            .is_some_and(|token| texts.iter().any(|text| token.is(text)))
    }

    /// Consume the next token when it is `text`.
    pub(super) fn eat(&mut self, text: &str) -> bool {
        if self.at(text) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn expect(&mut self, text: &str) -> Result<&'t Token, ParseError> {
        if self.at(text) {
            Ok(self.next().expect("peeked"))
        } else {
            Err(self.error(&format!("expected '{text}'{}", self.found())))
        }
    }

    pub(super) fn identifier(&mut self) -> Result<&'t Token, ParseError> {
        match self.peek() {
            Some(token) if token.kind == Kind::Identifier => Ok(self.next().expect("peeked")),
            _ => Err(self.error(&format!("expected an identifier{}", self.found()))),
        }
    }

    /// `, found 'x'` for a diagnostic, naming the next token.
    pub(super) fn found(&self) -> String {
        match self.peek() {
            Some(token) => format!(", found '{}'", token.text),
            None => ", found the end of the input".to_string(),
        }
    }

    pub(super) fn origin(&self) -> Origin {
        let token = self.peek().or_else(|| self.tokens.last());
        match token {
            Some(token) => Origin {
                file: token.location.file.clone(),
                line: token.location.line,
            },
            None => Origin {
                file: Rc::from("<input>"),
                line: 0,
            },
        }
    }

    /// An error at the next token.
    pub(super) fn error(&self, message: &str) -> ParseError {
        let origin = self.origin();
        ParseError {
            file: origin.file.to_string(),
            line: origin.line,
            message: message.to_string(),
        }
    }

    pub(super) fn error_at(&self, origin: &Origin, message: &str) -> ParseError {
        ParseError {
            file: origin.file.to_string(),
            line: origin.line,
            message: message.to_string(),
        }
    }

    pub(super) fn layout_error(&self, origin: &Origin, error: LayoutError) -> ParseError {
        self.error_at(origin, &error.0)
    }

    /// Skip a balanced `(…)`, `[…]` or `{…}` group starting at the next token.
    pub(super) fn skip_group(&mut self) -> Result<(), ParseError> {
        let origin = self.origin();
        let mut depth = 0usize;
        loop {
            let Some(token) = self.next() else {
                return Err(self.error_at(&origin, "unbalanced brackets"));
            };
            if token.kind != Kind::Punctuator {
                continue;
            }
            match &*token.text {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
    }

    /// Skip an initializer, up to the `,` or `;` that ends it.
    fn skip_initializer(&mut self) -> Result<(), ParseError> {
        while !self.at_any(&[",", ";"]) {
            if self.peek().is_none() {
                return Err(self.error("unterminated initializer"));
            }
            if self.at_any(&["(", "[", "{"]) {
                self.skip_group()?;
            } else {
                self.next();
            }
        }
        Ok(())
    }

    // ---- types --------------------------------------------------------------------------------

    pub(super) fn int_type(&mut self, rank: IntRank, signedness: Signedness) -> TypeId {
        self.declarations.intern(Type::Int { rank, signedness })
    }

    /// The typedef `name` names, when it is one.
    pub(super) fn typedef_named(&self, name: &str) -> Option<TypedefId> {
        match self.ordinary.get(name) {
            Some(Ordinary::Typedef(id)) => Some(*id),
            _ => None,
        }
    }

    // ---- declarations -------------------------------------------------------------------------

    pub(super) fn translation_unit(&mut self) -> Result<(), ParseError> {
        while self.peek().is_some() {
            self.external_declaration()?;
        }
        Ok(())
    }

    fn external_declaration(&mut self) -> Result<(), ParseError> {
        if self.eat(";") {
            return Ok(());
        }
        if self.at_any(&["_Static_assert", "static_assert"]) {
            return self.static_assertion();
        }
        if self.at_any(&["__asm__", "__asm", "asm"]) {
            // A file-scope `asm("…");` emits code and declares nothing.
            self.next();
            self.skip_group()?;
            self.expect(";")?;
            return Ok(());
        }
        let origin = self.origin();
        let specifiers = self.specifiers(true)?;
        if self.eat(";") {
            // `struct tag { … };` or `enum { … };`: the specifiers were the declaration.
            return Ok(());
        }
        loop {
            let declarator = self.declarator(DeclaratorKind::Named)?;
            let name_origin = declarator.origin.clone().unwrap_or_else(|| origin.clone());
            let ty = self.declared_type(&specifiers, &declarator, &name_origin)?;
            if self.at("{") {
                let Type::Function(_) = self.declarations.ty(ty) else {
                    return Err(self.error("a body may only follow a function declarator"));
                };
                self.skip_group()?;
                self.declare(&specifiers, &declarator, ty, true, name_origin)?;
                return Ok(());
            }
            self.declare(&specifiers, &declarator, ty, false, name_origin)?;
            if self.eat("=") {
                self.skip_initializer()?;
            }
            if self.eat(",") {
                continue;
            }
            self.expect(";")?;
            return Ok(());
        }
    }

    /// `_Static_assert(expression, message);`, which must hold.
    pub(super) fn static_assertion(&mut self) -> Result<(), ParseError> {
        let origin = self.origin();
        self.next();
        self.expect("(")?;
        let value = self.constant_expression()?;
        if self.eat(",") {
            while self
                .peek()
                .is_some_and(|token| token.kind == Kind::StringLiteral)
            {
                self.next();
            }
        }
        self.expect(")")?;
        self.expect(";")?;
        if value.is_zero() {
            return Err(self.error_at(&origin, "static assertion failed"));
        }
        Ok(())
    }

    /// Record what one declarator of a file-scope declaration declares.
    fn declare(
        &mut self,
        specifiers: &Specifiers,
        declarator: &Declarator,
        ty: TypeId,
        has_body: bool,
        origin: Origin,
    ) -> Result<(), ParseError> {
        let Some(name) = declarator.name.clone() else {
            return Err(self.error_at(&origin, "a declaration must name what it declares"));
        };
        let aligned = specifiers
            .attributes
            .aligned
            .max(declarator.attributes.aligned);
        if specifiers.storage == Some(StorageClass::Typedef) {
            return self.declare_typedef(name, ty, aligned, origin);
        }
        let storage = match specifiers.storage {
            Some(StorageClass::Extern) => Storage::Extern,
            Some(StorageClass::Static) => Storage::Static,
            _ => Storage::None,
        };
        match self.ordinary.get(&name) {
            Some(Ordinary::Typedef(_)) | Some(Ordinary::Constant(_)) => {
                return Err(self.error_at(
                    &origin,
                    &format!("'{name}' redeclared as a different kind of symbol"),
                ));
            }
            _ => {}
        }
        self.ordinary.insert(name.clone(), Ordinary::Object(ty));
        let asm_name = declarator.asm_name.clone();
        if let Type::Function(_) = self.declarations.ty(ty) {
            let noreturn = specifiers.noreturn
                || specifiers.attributes.noreturn
                || declarator.attributes.noreturn;
            if let Some(&index) = self.functions.get(&name) {
                let existing = &mut self.declarations.functions[index];
                existing.has_body |= has_body;
                existing.inline |= specifiers.inline;
                existing.noreturn |= noreturn;
                if existing.asm_name.is_none() {
                    existing.asm_name = asm_name;
                }
                if has_body || existing.param_names.iter().all(Option::is_none) {
                    existing.param_names = declarator.param_names.clone();
                }
                return Ok(());
            }
            self.functions
                .insert(name.clone(), self.declarations.functions.len());
            self.declarations.functions.push(Function {
                name,
                ty,
                param_names: declarator.param_names.clone(),
                storage,
                inline: specifiers.inline,
                noreturn,
                has_body,
                asm_name,
                origin,
            });
            return Ok(());
        }
        if let Some(&index) = self.variables.get(&name) {
            let existing = &mut self.declarations.variables[index];
            if existing.asm_name.is_none() {
                existing.asm_name = asm_name;
            }
            // A later declaration may complete an array's length (`extern int t[]; int t[4];`).
            existing.ty = ty;
            return Ok(());
        }
        self.variables
            .insert(name.clone(), self.declarations.variables.len());
        self.declarations.variables.push(Variable {
            name,
            ty,
            storage,
            thread_local: specifiers.thread_local,
            aligned,
            asm_name,
            origin,
        });
        Ok(())
    }

    fn declare_typedef(
        &mut self,
        name: Rc<str>,
        ty: TypeId,
        aligned: Option<u64>,
        origin: Origin,
    ) -> Result<(), ParseError> {
        match self.ordinary.get(&name) {
            // C11 allows a typedef to be repeated with the same type.
            Some(Ordinary::Typedef(existing)) => {
                let existing = self.declarations.typedef(*existing);
                if self.declarations.canonical(existing.ty) != self.declarations.canonical(ty)
                    || existing.aligned != aligned
                {
                    return Err(self.error_at(
                        &origin,
                        &format!(
                            "typedef redefinition with different types ('{}' vs '{}')",
                            self.declarations.spell(ty),
                            self.declarations.spell(existing.ty)
                        ),
                    ));
                }
                Ok(())
            }
            Some(_) => Err(self.error_at(
                &origin,
                &format!("'{name}' redeclared as a different kind of symbol"),
            )),
            None => {
                let id = self.declarations.add_typedef(Typedef {
                    name: name.clone(),
                    ty,
                    aligned,
                    origin: Some(origin),
                });
                self.ordinary.insert(name, Ordinary::Typedef(id));
                Ok(())
            }
        }
    }
}
