//! Declarators, parameter lists and type names (C11 §6.7.6–6.7.7): the part of a declaration that
//! derives pointer, array and function types from the specified type, inside out.

use std::rc::Rc;

use super::super::lexer::Kind;
use super::super::model::{FunctionType, Origin, Qualifiers, Type, TypeId};
use super::attributes::Attributes;
use super::parser::Parser;
use super::specifiers::Specifiers;
use super::ParseError;

/// Where a declarator is, which decides whether it may or must name something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeclaratorKind {
    /// A file-scope declaration or a struct member.
    Named,
    /// A parameter, which may be named or abstract.
    Parameter,
    /// A type name, which names nothing.
    Abstract,
}

/// One parenthesization level of a declarator: the pointers written before it and the array and
/// function suffixes after it.
#[derive(Debug, Default)]
struct Level {
    pointers: Vec<Qualifiers>,
    suffixes: Vec<Suffix>,
}

#[derive(Debug)]
enum Suffix {
    Array(Option<u64>),
    Function {
        params: Vec<TypeId>,
        names: Vec<Option<Rc<str>>>,
        variadic: bool,
        prototyped: bool,
    },
}

/// A parsed declarator; [`Parser::declared_type`] applies it to a specified type.
#[derive(Debug)]
pub(super) struct Declarator {
    pub(super) name: Option<Rc<str>>,
    /// Where the name is.
    pub(super) origin: Option<Origin>,
    /// Outermost first; the name, if any, is in the last.
    levels: Vec<Level>,
    pub(super) attributes: Attributes,
    pub(super) asm_name: Option<Rc<str>>,
    /// The parameter names, when the declarator declares a function.
    pub(super) param_names: Vec<Option<Rc<str>>>,
}

impl Parser<'_> {
    pub(super) fn empty_declarator(&self) -> Declarator {
        Declarator {
            name: None,
            origin: None,
            levels: vec![Level::default()],
            attributes: Attributes::default(),
            asm_name: None,
            param_names: Vec::new(),
        }
    }

    pub(super) fn declarator(&mut self, kind: DeclaratorKind) -> Result<Declarator, ParseError> {
        let mut declarator = self.empty_declarator();
        declarator.levels.clear();
        self.declarator_level(kind, &mut declarator)?;
        loop {
            if self.at_attribute() {
                self.attributes(&mut declarator.attributes)?;
            } else if let Some(label) = self.asm_label()? {
                declarator.asm_name = Some(label);
            } else {
                break;
            }
        }
        // The derivation applied last is the one closest to the name.
        for level in declarator.levels.iter().rev() {
            if let Some(first) = level.suffixes.first() {
                if let Suffix::Function { names, .. } = first {
                    declarator.param_names = names.clone();
                }
                break;
            }
            if !level.pointers.is_empty() {
                break;
            }
        }
        Ok(declarator)
    }

    fn declarator_level(
        &mut self,
        kind: DeclaratorKind,
        declarator: &mut Declarator,
    ) -> Result<(), ParseError> {
        let mut pointers = Vec::new();
        while self.eat("*") {
            let mut qualifiers = Qualifiers::default();
            while let Some(token) = self.peek() {
                let qualifier = match &*token.text {
                    "const" | "__const" | "__const__" => Qualifiers::CONST,
                    "volatile" | "__volatile" | "__volatile__" => Qualifiers::VOLATILE,
                    "restrict" | "__restrict" | "__restrict__" => Qualifiers::RESTRICT,
                    "__attribute__" | "__attribute" => {
                        self.attributes(&mut declarator.attributes)?;
                        continue;
                    }
                    _ => break,
                };
                qualifiers = qualifiers.union(qualifier);
                self.next();
            }
            pointers.push(qualifiers);
        }
        let index = declarator.levels.len();
        declarator.levels.push(Level {
            pointers,
            suffixes: Vec::new(),
        });
        self.attributes(&mut declarator.attributes)?;
        match self.peek() {
            Some(token) if token.kind == Kind::Identifier && kind != DeclaratorKind::Abstract => {
                if declarator.name.is_some() {
                    return Err(self.error("a declarator may name one thing"));
                }
                declarator.origin = Some(self.origin());
                declarator.name = Some(token.text.clone());
                self.next();
            }
            Some(token) if token.is("(") && self.opens_nested_declarator(kind) => {
                self.next();
                self.declarator_level(kind, declarator)?;
                self.expect(")")?;
            }
            _ => {}
        }
        loop {
            let suffix = if self.at("[") {
                self.array_suffix(kind)?
            } else if self.at("(") {
                self.function_suffix()?
            } else {
                break;
            };
            declarator.levels[index].suffixes.push(suffix);
        }
        Ok(())
    }

    /// At a `(` where a direct declarator may start: whether it parenthesizes a declarator
    /// (`(*p)`) rather than opening a parameter list (`(int)`, `()`).
    fn opens_nested_declarator(&self, kind: DeclaratorKind) -> bool {
        let Some(next) = self.peek_at(1) else {
            return false;
        };
        if next.is("*") || next.is("(") || next.is("[") || next.is("^") {
            return true;
        }
        if next.is("__attribute__") || next.is("__attribute") {
            return true;
        }
        next.kind == Kind::Identifier
            && kind != DeclaratorKind::Abstract
            && !self.begins_specifiers(next)
    }

    /// `[` [qualifiers] [`static`] [length] `]`. A parameter's array decays to a pointer, so its
    /// length may be any expression (`char buf[n]`); elsewhere it must be a constant.
    fn array_suffix(&mut self, kind: DeclaratorKind) -> Result<Suffix, ParseError> {
        self.next();
        while self.at_any(&[
            "static",
            "const",
            "__const",
            "volatile",
            "restrict",
            "__restrict",
            "__restrict__",
        ]) {
            self.next();
        }
        if self.eat("]") {
            return Ok(Suffix::Array(None));
        }
        if self.at("*") && self.peek_at(1).is_some_and(|next| next.is("]")) {
            self.next();
            self.next();
            return Ok(Suffix::Array(None));
        }
        let start = self.position();
        let origin = self.origin();
        match self.constant_expression() {
            Ok(length) if self.at("]") => {
                self.next();
                if length.is_negative() {
                    return Err(self.error_at(&origin, "array has a negative size"));
                }
                Ok(Suffix::Array(Some(length.as_u64())))
            }
            Ok(_) => Err(self.error(&format!("expected ']'{}", self.found()))),
            Err(_) if kind == DeclaratorKind::Parameter => {
                self.rewind(start);
                let mut depth = 0usize;
                loop {
                    let Some(token) = self.next() else {
                        return Err(self.error_at(&origin, "expected ']'"));
                    };
                    if token.is("[") {
                        depth += 1;
                    } else if token.is("]") {
                        if depth == 0 {
                            return Ok(Suffix::Array(None));
                        }
                        depth -= 1;
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    /// `(` parameters `)`: `()` declares nothing about them; `(void)` declares none.
    fn function_suffix(&mut self) -> Result<Suffix, ParseError> {
        self.next();
        let mut params = Vec::new();
        let mut names = Vec::new();
        if self.eat(")") {
            return Ok(Suffix::Function {
                params,
                names,
                variadic: false,
                prototyped: false,
            });
        }
        if self.at("void") && self.peek_at(1).is_some_and(|next| next.is(")")) {
            self.next();
            self.next();
            return Ok(Suffix::Function {
                params,
                names,
                variadic: false,
                prototyped: true,
            });
        }
        let mut variadic = false;
        loop {
            if self.eat("...") {
                variadic = true;
                self.expect(")")?;
                break;
            }
            let origin = self.origin();
            let specifiers = self.specifiers(true)?;
            let declarator = self.declarator(DeclaratorKind::Parameter)?;
            let ty = self.declared_type(&specifiers, &declarator, &origin)?;
            params.push(self.adjust_parameter(ty));
            names.push(declarator.name);
            if self.eat(",") {
                continue;
            }
            self.expect(")")?;
            break;
        }
        Ok(Suffix::Function {
            params,
            names,
            variadic,
            prototyped: true,
        })
    }

    /// A parameter's type as the function's type has it (C11 §6.7.6.3p7–8, p15): an array is a
    /// pointer to its element, a function a pointer to it, and top-level qualifiers drop.
    fn adjust_parameter(&mut self, ty: TypeId) -> TypeId {
        let unqualified = match self.declarations.ty(ty) {
            Type::Qualified { base, .. } => *base,
            _ => ty,
        };
        match self
            .declarations
            .ty(self.declarations.canonical(unqualified))
        {
            Type::Array { element, .. } => {
                let element = *element;
                self.declarations.intern(Type::Pointer(element))
            }
            Type::Function(_) => self.declarations.intern(Type::Pointer(unqualified)),
            _ => unqualified,
        }
    }

    /// The type `declarator` gives a declaration whose specifiers are `specifiers`.
    pub(super) fn declared_type(
        &mut self,
        specifiers: &Specifiers,
        declarator: &Declarator,
        origin: &Origin,
    ) -> Result<TypeId, ParseError> {
        let mut attributes = specifiers.attributes.clone();
        attributes.merge(&declarator.attributes);
        let mut ty = self
            .apply_type_attributes(specifiers.ty, &attributes)
            .map_err(|error| ParseError {
                file: origin.file.to_string(),
                line: origin.line,
                ..error
            })?;
        for level in &declarator.levels {
            for &qualifiers in &level.pointers {
                ty = self.declarations.intern(Type::Pointer(ty));
                if !qualifiers.is_empty() {
                    ty = self.declarations.intern(Type::Qualified {
                        base: ty,
                        qualifiers,
                    });
                }
            }
            for suffix in level.suffixes.iter().rev() {
                ty = match suffix {
                    Suffix::Array(length) => self.declarations.intern(Type::Array {
                        element: ty,
                        length: *length,
                    }),
                    Suffix::Function {
                        params,
                        variadic,
                        prototyped,
                        ..
                    } => self.declarations.intern(Type::Function(FunctionType {
                        result: ty,
                        params: params.clone(),
                        variadic: *variadic,
                        prototyped: *prototyped,
                    })),
                };
            }
        }
        Ok(ty)
    }

    /// A type name (C11 §6.7.7): specifiers and an abstract declarator, as in a cast or `sizeof`.
    pub(super) fn type_name(&mut self) -> Result<TypeId, ParseError> {
        let origin = self.origin();
        let specifiers = self.specifiers(false)?;
        let declarator = self.declarator(DeclaratorKind::Abstract)?;
        self.declared_type(&specifiers, &declarator, &origin)
    }
}
