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

use super::library_members::{
    library_member_identity, library_member_owner, library_member_signature, MemberRole,
};
use super::*;

/// Whether `classifier` is `kotlin.text.StringBuilder`.
pub(in crate::codegen) fn is_string_builder(classifier: TypeName) -> bool {
    crate::string_builders::lays_out(classifier)
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
    /// `append(CharSequence?, Int, Int)`: a range of the sequence.
    AppendRange,
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
        ("append", [value, Ty::Int, Ty::Int])
            if appended(*value) == Some(Appended::CharSequence) =>
        {
            Some(Member::AppendRange)
        }
        ("<get-length>", []) if fact.ret == Ty::Int => Some(Member::Length),
        ("get", [Ty::Int]) if fact.ret == Ty::Char => Some(Member::Get),
        _ => None,
    }
}

fn builder_type() -> Ty {
    Ty::obj_name(krusty::types::wk::string_builder())
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
        self.runtime.string_builder_constructed();
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
                // `StringBuilder(content)` appends `content`; WASI first sizes the builder by
                // `content.length`, a read a custom sequence observes.
                let append = self.char_sequence_append()?;
                let sequence = self.local(REFERENCE);
                self.value(*content, *param)?;
                self.code.local_set(sequence);
                match self.runtime.char_sequence_length(self.module) {
                    Some(length) => self.code.local_get(sequence).call(length),
                    None => self.code.i32_const(0),
                };
                self.code.call(new).local_get(sequence).call(append);
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
                let append = self.char_sequence_append()?;
                self.value(*value, fact.params[0])?;
                self.code.call(append);
            }
            (Member::AppendRange, [value, start, end]) => {
                let append = self.char_sequence_range_append()?;
                self.value(*value, fact.params[0])?;
                self.value(*start, Ty::Int)?;
                self.value(*end, Ty::Int)?;
                self.code.call(append);
            }
            (Member::Length, []) => {
                self.code.call(length);
            }
            (Member::Get, [index]) => {
                // `StringBuilder.get` throws for an index outside the units in use.
                let (builder, offset) = (self.local(REFERENCE), self.local(ValType::I32));
                self.code.local_set(builder);
                self.value(*index, Ty::Int)?;
                self.code.local_tee(offset).local_get(builder).call(length);
                self.throw_unless_index_below()?;
                self.code.local_get(builder).local_get(offset).call(get);
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

    /// The runtime's `StringBuilder.append(CharSequence?)` (`crate::char_sequence_appends`).
    fn char_sequence_append(&mut self) -> Result<u32, Unsupported> {
        self.record_char_sequence_reads()?;
        self.runtime.append_char_sequence(self.module)
    }

    /// The runtime's `StringBuilder.append(CharSequence?, Int, Int)`
    /// (`crate::char_sequence_appends`).
    fn char_sequence_range_append(&mut self) -> Result<u32, Unsupported> {
        self.record_char_sequence_reads()?;
        self.runtime.append_char_sequence_range(self.module)
    }

    /// On a target whose builder copies an appended sequence's units, record the checked
    /// `CharSequence.length` and `get` it reads them through, with what a string and the builder
    /// run for each. A file without them records nothing, and an append declines.
    pub(super) fn record_char_sequence_reads(&mut self) -> Result<(), Unsupported> {
        let appends = &self.runtime.char_sequence_appends;
        if !appends.reads_by_index() || appends.has_reads() {
            return Ok(());
        }
        let Some(protocol) = self.callables.char_sequence_protocol() else {
            return Ok(());
        };
        let property = self
            .callables
            .property(protocol.length)
            .ok_or("a `CharSequence.length` without its declaration")?;
        let (getter, name) = (property.getter, property.name.to_string());
        let length = self.char_sequence_member(getter, MemberRole::Getter, &name)?;
        let get = self.char_sequence_member(protocol.get, MemberRole::Function, "get")?;
        self.runtime
            .char_sequence_appends
            .record_reads(crate::char_sequence_appends::SequenceReads { length, get });
        Ok(())
    }

    /// The library member `target` is in `role`, with its builtin implementations recorded.
    fn char_sequence_member(
        &mut self,
        target: ExternalCallableId,
        role: MemberRole,
        name: &str,
    ) -> Result<crate::interface_dispatch::MemberRef, Unsupported> {
        let fact = self
            .callables
            .callable(target)
            .ok_or("a `CharSequence` member without its declaration")?
            .clone();
        let identity = library_member_identity(&fact, role)
            .ok_or("a `CharSequence` member without its identity")?;
        let owner =
            library_member_owner(&fact).ok_or("a `CharSequence` member without its classifier")?;
        let signature = library_member_signature(&fact, role)?;
        let member = self.runtime.objects.interfaces.library(
            self.module,
            owner,
            &identity,
            signature.clone(),
        )?;
        self.runtime.builtin_member(
            self.module,
            member,
            name,
            &signature,
            &fact.builtin_implementations,
        )?;
        Ok(member)
    }
}
