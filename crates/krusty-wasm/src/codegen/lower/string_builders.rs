//! Constructions and member calls of `kotlin.text.StringBuilder`, which the runtime lays out
//! (`crate::string_builders`).
//!
//! The selected library declaration says which member a call is: its declaring classifier, its
//! name and its declared parameters, as the provider froze them. The receiver's representation is
//! the runtime's, so each member is the runtime function realizing it at that receiver. A member
//! the runtime does not realize declines the file.

use krusty::backend::BackendCallableFact;
use krusty::fir::ExternalCallableId;
use krusty::types::{SemanticCallableOwner, TypeName};

use super::objects::STRING_HEAP;
use super::*;

/// Whether `classifier` is `kotlin.text.StringBuilder`.
pub(in crate::codegen) fn is_string_builder(classifier: TypeName) -> bool {
    classifier.matches("kotlin/text/StringBuilder")
}

/// Whether `ty` is a `StringBuilder`, nullable or not.
pub(super) fn is_string_builder_type(ty: Ty) -> bool {
    matches!(ty.non_null().kind(), TyKind::Obj(name, _) if is_string_builder(name))
}

/// Whether the selected declaration `fact` is declared by `StringBuilder`.
pub(in crate::codegen) fn declared_by_string_builder(fact: &BackendCallableFact) -> bool {
    matches!(
        fact.declaration_owner,
        Some(SemanticCallableOwner::Classifier(owner)) if is_string_builder(owner)
    )
}

/// What a value appended to a builder is, by the parameter the selected `append` declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Appended {
    /// A `String?`, appended as itself or as `"null"`.
    String,
    /// A `Char`, appended as its one unit.
    Char,
    /// A `CharSequence?`, appended as the string it renders, or as `"null"`.
    CharSequence,
}

/// One member of `StringBuilder` the runtime realizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Member {
    Append(Appended),
    Length,
    Get,
}

/// The parameter type an `append` overload declares, as what it appends.
fn appended(param: Ty) -> Option<Appended> {
    if param == Ty::nullable(Ty::String) {
        return Some(Appended::String);
    }
    if param == Ty::Char {
        return Some(Appended::Char);
    }
    match param.kind() {
        TyKind::Nullable(inner) if is_char_sequence(inner) => Some(Appended::CharSequence),
        _ => None,
    }
}

/// Whether `ty` is the non-null `CharSequence`.
fn is_char_sequence(ty: Ty) -> bool {
    matches!(ty.kind(), TyKind::Obj(name, _) if name == krusty::types::wk::char_sequence())
}

/// The member the selected declaration `fact` of `StringBuilder` is.
fn member(fact: &BackendCallableFact) -> Option<Member> {
    match (fact.name.as_str(), fact.params.as_slice()) {
        ("append", [param]) => appended(*param).map(Member::Append),
        ("<get-length>", []) if fact.ret == Ty::Int => Some(Member::Length),
        ("get", [Ty::Int]) if fact.ret == Ty::Char => Some(Member::Get),
        _ => None,
    }
}

fn builder_type() -> Ty {
    Ty::obj("kotlin/text/StringBuilder")
}

impl Body<'_> {
    /// `StringBuilder(...)` through the selected library constructor `target`.
    pub(super) fn string_builder_construction(
        &mut self,
        target: ExternalCallableId,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let fact = self
            .callables
            .callable(target)
            .ok_or_else(|| "a StringBuilder constructor without its declaration".to_string())?;
        if fact.kind != krusty::libraries::ExternalCallableKind::Constructor
            || !declared_by_string_builder(fact)
            || fact.params.len() != args.len()
        {
            return Err("a StringBuilder construction disagreeing with its constructor".into());
        }
        let builders = &self.runtime.string_builders;
        let (new, append_string) = (builders.new, builders.append_string);
        match (fact.params.as_slice(), args) {
            ([], []) => {
                self.code.i32_const(0).call(new);
            }
            ([Ty::Int], [capacity]) => {
                self.value(*capacity, Ty::Int)?;
                self.code.call(new);
            }
            ([param], [content]) if *param == Ty::String => {
                self.code.i32_const(0).call(new);
                self.value(*content, Ty::nullable(Ty::String))?;
                self.code.call(append_string);
            }
            ([param], [content]) if is_char_sequence(*param) => {
                self.code.i32_const(0).call(new);
                self.value(*content, *param)?;
                self.rendered_char_sequence();
                self.code.call(append_string);
            }
            _ => return Err("a StringBuilder constructor the runtime does not realize".into()),
        }
        Ok(())
    }

    /// A call of the selected library member `target` of `StringBuilder` on `receiver`.
    pub(super) fn string_builder_call(
        &mut self,
        target: ExternalCallableId,
        receiver: u32,
        args: &[u32],
    ) -> Result<(), Unsupported> {
        let fact = self
            .callables
            .callable(target)
            .ok_or_else(|| "a StringBuilder member without its declaration".to_string())?;
        let Some(member) = member(fact) else {
            return Err(format!(
                "the StringBuilder member `{}`, which the runtime does not realize",
                fact.name
            ));
        };
        if fact.params.len() != args.len() {
            return Err("a StringBuilder call disagreeing with its member".into());
        }
        self.value(receiver, builder_type())?;
        let builders = &self.runtime.string_builders;
        let (append_string, append_char, length, get) = (
            builders.append_string,
            builders.append_char,
            builders.length,
            builders.get,
        );
        match (member, args) {
            (Member::Append(Appended::String), [value]) => {
                self.value(*value, Ty::nullable(Ty::String))?;
                self.code.call(append_string);
            }
            (Member::Append(Appended::Char), [value]) => {
                self.value(*value, Ty::Char)?;
                self.code.call(append_char);
            }
            (Member::Append(Appended::CharSequence), [value]) => {
                self.value(*value, fact.params[0])?;
                self.rendered_char_sequence();
                self.code.call(append_string);
            }
            (Member::Length, []) => {
                self.code.call(length);
            }
            (Member::Get, [index]) => {
                self.value(*index, Ty::Int)?;
                self.code.call(get);
            }
            _ => return Err("a StringBuilder call with malformed operands".into()),
        }
        Ok(())
    }

    /// A read of the library property `target` of `StringBuilder` on `receiver`.
    pub(super) fn string_builder_property(
        &mut self,
        target: krusty::fir::ExternalPropertyId,
        receiver: u32,
    ) -> Result<(), Unsupported> {
        let getter = self
            .callables
            .property(target)
            .map(|property| property.getter)
            .ok_or_else(|| "a StringBuilder property without its declaration".to_string())?;
        self.string_builder_call(getter, receiver, &[])
    }

    /// Whether `target` is a property declared by `StringBuilder`.
    pub(super) fn is_string_builder_property(
        &self,
        target: krusty::fir::ExternalPropertyId,
    ) -> bool {
        self.callables
            .property(target)
            .and_then(|property| self.callables.callable(property.getter))
            .is_some_and(declared_by_string_builder)
    }

    /// Replace the nullable `CharSequence` on the stack with the string it renders: itself for a
    /// string, `"null"` for `null`, and otherwise its own `toString`, which a builder's table
    /// answers with its contents.
    fn rendered_char_sequence(&mut self) {
        let value = self.local(REFERENCE);
        self.code
            .local_tee(value)
            .ref_test(false, STRING_HEAP)
            .if_(BlockType::Value(STRING))
            .local_get(value)
            .ref_cast(false, STRING_HEAP)
            .else_()
            .local_get(value);
        self.dispatch_to_string();
        self.code.end();
    }
}
