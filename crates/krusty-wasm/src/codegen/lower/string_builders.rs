//! Constructions and member calls of `kotlin.text.StringBuilder`, which the runtime lays out
//! (`crate::string_builders`).
//!
//! The selected library declaration says which member a call is: its declaring classifier, its
//! name and its declared parameters, as the provider froze them. The receiver's representation is
//! the runtime's, so each member is the runtime function realizing it at that receiver. A member
//! the runtime does not realize declines the file.

use krusty::backend::BackendCallableFact;
use krusty::fir::ExternalCallableId;
use krusty::libraries::RuntimeExceptionKind;
use krusty::types::{SemanticCallableOwner, TypeName};

use super::library_members::{library_member_identity, MemberRole};
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
    /// An `Any?`, appended as its `toString()`, or as `"null"`.
    Any,
}

/// One member of `StringBuilder` the runtime realizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Member {
    Append(Appended),
    /// `append(CharSequence?, Int, Int)`: a range of the sequence.
    AppendRange,
    /// `appendRange(value, startIndex, endIndex)` of the declared `value` type: a `CharSequence`
    /// or a `CharArray`.
    AppendRangeOf(Ty),
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
        TyKind::Nullable(inner) if is_any(inner) => Some(Appended::Any),
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
        ("appendRange", [value, Ty::Int, Ty::Int]) if fact.ret == builder_type() => {
            Some(Member::AppendRangeOf(*value))
        }
        ("<get-length>", []) if fact.ret == Ty::Int => Some(Member::Length),
        ("get", [Ty::Int]) if fact.ret == Ty::Char => Some(Member::Get),
        _ => None,
    }
}

/// Whether `ty` is the non-null `Any`.
fn is_any(ty: Ty) -> bool {
    matches!(ty.kind(), TyKind::Obj(name, _) if name == krusty::types::wk::any())
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
            (Member::Append(Appended::Any), [value]) => {
                // `append(value: Any?)` appends `value.toString()`, or `"null"`.
                self.value(*value, fact.params[0])?;
                self.stacked_string_of(fact.params[0])?;
                self.code.call(append_string);
            }
            (Member::Append(Appended::CharSequence), [value]) => {
                let append = self.char_sequence_append()?;
                self.value(*value, fact.params[0])?;
                self.code.call(append);
            }
            (Member::AppendRange, [value, start, end]) => {
                let append = self.char_sequence_range_append()?;
                // The receiver, the sequence and both endpoints are evaluated, in that order,
                // before the range check.
                let builder = self.local(REFERENCE);
                let (sequence, from, to) = (
                    self.local(REFERENCE),
                    self.local(ValType::I32),
                    self.local(ValType::I32),
                );
                self.code.local_set(builder);
                self.value(*value, fact.params[0])?;
                self.code.local_set(sequence);
                self.value(*start, Ty::Int)?;
                self.code.local_set(from);
                self.value(*end, Ty::Int)?;
                self.code.local_set(to);
                self.check_char_sequence_range(sequence, from, to)?;
                self.code
                    .local_get(builder)
                    .local_get(sequence)
                    .local_get(from)
                    .local_get(to)
                    .call(append);
            }
            (Member::AppendRangeOf(declared), [value, start, end]) => {
                self.append_range(declared, *value, *start, *end)?;
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

    /// The range check of `StringBuilder.append(CharSequence?, Int, Int)` on both Wasm targets,
    /// before anything is rendered or written: one read of the original sequence's `length`
    /// (`"null".length` for `null`), then `AbstractList.checkRangeIndexes`, which throws
    /// `IndexOutOfBoundsException` for a negative start or an end past the length, and
    /// `IllegalArgumentException` for a reversed range.
    fn check_char_sequence_range(
        &mut self,
        sequence: u32,
        from: u32,
        to: u32,
    ) -> Result<(), Unsupported> {
        let length = self
            .runtime
            .char_sequence_range_length(self.module)
            .ok_or("a ranged append of a `CharSequence` without its checked `length`")?;
        let size = self.local(ValType::I32);
        self.code
            .i32_const("null".len() as i32)
            .local_set(size)
            .local_get(sequence)
            .ref_is_null()
            .op(EQZ32)
            .if_(BlockType::Empty)
            .local_get(sequence)
            .call(length)
            .local_set(size)
            .end();
        let out_of_bounds = self.runtime.exception_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IndexOutOfBounds.classifier(),
        )?;
        self.code
            .local_get(from)
            .i32_const(0)
            .op(LT_S32)
            .local_get(to)
            .local_get(size)
            .op(GT_S32)
            .op(OR32)
            .if_(BlockType::Empty);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            out_of_bounds,
            None,
            self.runtime.string,
        )?;
        self.code.throw(self.runtime.exceptions.tag).end();
        let reversed = self.runtime.exception_class(
            self.module,
            self.classifiers,
            RuntimeExceptionKind::IllegalArgument.classifier(),
        )?;
        self.code
            .local_get(from)
            .local_get(to)
            .op(GT_S32)
            .if_(BlockType::Empty);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            reversed,
            None,
            self.runtime.string,
        )?;
        self.code.throw(self.runtime.exceptions.tag).end();
        Ok(())
    }

    /// `appendRange(value, start, end)` on the builder on the stack, leaving the builder.
    ///
    /// Both Wasm stdlibs first check the range as `AbstractList.checkBoundsIndexes(start, end,
    /// value.length)` does. Kotlin/Wasm JS then appends that range of `value.toString()`; WASI
    /// copies a `String`'s range whole and reads every other sequence one `value[index]` at a
    /// time. A `CharArray`'s units are copied on both.
    fn append_range(
        &mut self,
        declared: Ty,
        value: u32,
        start: u32,
        end: u32,
    ) -> Result<(), Unsupported> {
        let builder = self.local(REFERENCE);
        self.code.local_set(builder);
        let (first, last, size) = (
            self.local(ValType::I32),
            self.local(ValType::I32),
            self.local(ValType::I32),
        );
        if is_char_sequence(declared) {
            let reads = self.char_sequence_reads()?;
            let sequence = self.local(REFERENCE);
            self.value(value, declared)?;
            self.code.local_set(sequence);
            self.value(start, Ty::Int)?;
            self.code.local_set(first);
            self.value(end, Ty::Int)?;
            self.code.local_set(last);
            let length = self
                .runtime
                .objects
                .interfaces
                .stub(self.module, reads.length);
            self.code.local_get(sequence).call(length).local_set(size);
            self.check_bounds_indexes(first, last, size)?;
            self.append_sequence_range(builder, sequence, first, last, reads);
            return Ok(());
        }
        let (form, element) = self.array_form(declared)?;
        if element != Ty::Char {
            return Err("an `appendRange` of an array other than `CharArray`".into());
        }
        let storage = self.local(ValType::reference(form.storage));
        self.value(value, declared)?;
        self.array_storage(form);
        self.code.local_set(storage);
        self.value(start, Ty::Int)?;
        self.code.local_set(first);
        self.value(end, Ty::Int)?;
        self.code.local_set(last);
        self.code.local_get(storage).array_len().local_set(size);
        self.check_bounds_indexes(first, last, size)?;
        let append_char = self.runtime.string_builders.append_char;
        self.code
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(first)
            .local_get(last)
            .op(GE_S32)
            .br_if(1)
            .local_get(builder)
            .local_get(storage)
            .local_get(first)
            .array_get_u(form.storage)
            .call(append_char)
            .drop_()
            .local_get(first)
            .i32_const(1)
            .op(ADD32)
            .local_set(first)
            .br(0)
            .end()
            .end()
            .local_get(builder);
        Ok(())
    }

    /// Append `sequence[first, last)` to `builder` once the range is checked, as the target's
    /// stdlib reads it, leaving the builder.
    fn append_sequence_range(
        &mut self,
        builder: u32,
        sequence: u32,
        first: u32,
        last: u32,
        reads: crate::char_sequence_appends::SequenceReads,
    ) {
        let append_range = self.runtime.string_builders.append_range;
        if !self.runtime.char_sequence_appends.reads_by_index() {
            self.code
                .local_get(builder)
                .local_get(sequence)
                .call(self.runtime.object_strings.any_to_string)
                .local_get(first)
                .local_get(last)
                .call(append_range);
            return;
        }
        let string = HeapType::Concrete(self.runtime.string);
        // A string's range is copied whole, as `insertString` does.
        self.code
            .local_get(sequence)
            .ref_test(false, string)
            .if_(BlockType::Empty)
            .local_get(builder)
            .local_get(sequence)
            .ref_cast(false, string)
            .local_get(first)
            .local_get(last)
            .call(append_range)
            .drop_()
            .else_();
        let (get, append_char) = (
            self.runtime.objects.interfaces.stub(self.module, reads.get),
            self.runtime.string_builders.append_char,
        );
        self.code
            .block(BlockType::Empty)
            .loop_(BlockType::Empty)
            .local_get(first)
            .local_get(last)
            .op(GE_S32)
            .br_if(1)
            .local_get(builder)
            .local_get(sequence)
            .local_get(first)
            .call(get)
            .call(append_char)
            .drop_()
            .local_get(first)
            .i32_const(1)
            .op(ADD32)
            .local_set(first)
            .br(0)
            .end()
            .end()
            .end()
            .local_get(builder);
    }

    /// `AbstractList.checkBoundsIndexes(first, last, size)`: `IndexOutOfBoundsException` for a
    /// range reaching outside `0..size`, then `IllegalArgumentException` for a reversed one, each
    /// worded as the Wasm stdlib words it.
    fn check_bounds_indexes(
        &mut self,
        first: u32,
        last: u32,
        size: u32,
    ) -> Result<(), Unsupported> {
        self.code
            .local_get(first)
            .i32_const(0)
            .op(LT_S32)
            .local_get(last)
            .local_get(size)
            .op(GT_S32)
            .op(OR32)
            .if_(BlockType::Empty);
        self.throw_worded(
            RuntimeExceptionKind::IndexOutOfBounds,
            &[
                ("startIndex: ", first),
                (", endIndex: ", last),
                (", size: ", size),
            ],
        )?;
        self.code
            .end()
            .local_get(first)
            .local_get(last)
            .op(GT_S32)
            .if_(BlockType::Empty);
        self.throw_worded(
            RuntimeExceptionKind::IllegalArgument,
            &[("startIndex: ", first), (" > endIndex: ", last)],
        )?;
        self.code.end();
        Ok(())
    }

    /// Throw the runtime exception `kind` whose message is each text followed by its `Int`
    /// local's decimal rendering.
    fn throw_worded(
        &mut self,
        kind: RuntimeExceptionKind,
        parts: &[(&str, u32)],
    ) -> Result<(), Unsupported> {
        let structure =
            self.runtime
                .exception_class(self.module, self.classifiers, kind.classifier())?;
        for (index, (text, value)) in parts.iter().enumerate() {
            self.string_constant(text);
            if index > 0 {
                self.code.call(self.runtime.string_concat);
            }
            self.code
                .local_get(*value)
                .call(self.runtime.string_of_i32)
                .call(self.runtime.string_concat);
        }
        let message = self.local(STRING);
        self.code.local_set(message);
        self.runtime.exceptions.construct_class(
            &mut self.code,
            structure,
            Some(message),
            self.runtime.string,
        )?;
        self.code.throw(self.runtime.exceptions.tag);
        Ok(())
    }

    /// The runtime's `StringBuilder.append(CharSequence?)` (`crate::char_sequence_appends`).
    fn char_sequence_append(&mut self) -> Result<u32, Unsupported> {
        self.record_char_sequence_reads(false)?;
        self.runtime.append_char_sequence(self.module)
    }

    /// The runtime's `StringBuilder.append(CharSequence?, Int, Int)`
    /// (`crate::char_sequence_appends`).
    fn char_sequence_range_append(&mut self) -> Result<u32, Unsupported> {
        // Both targets read the sequence's `length` before the range check.
        self.record_char_sequence_reads(true)?;
        self.runtime.append_char_sequence_range(self.module)
    }

    /// On a target whose builder copies an appended sequence's units, record the checked
    /// `CharSequence.length` and `get` it reads them through, with what a string and the builder
    /// run for each. A file without them records nothing, and an append declines.
    ///
    /// `on_every_target` also records them where the builder renders an appended sequence, for the
    /// ranged append that reads its `length` there too.
    pub(super) fn record_char_sequence_reads(
        &mut self,
        on_every_target: bool,
    ) -> Result<(), Unsupported> {
        let appends = &self.runtime.char_sequence_appends;
        if !(appends.reads_by_index() || on_every_target) || appends.has_reads() {
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

    /// The checked `CharSequence.length` and `get` an `appendRange` reads on either target.
    fn char_sequence_reads(
        &mut self,
    ) -> Result<crate::char_sequence_appends::SequenceReads, Unsupported> {
        self.record_char_sequence_reads(true)?;
        self.runtime.char_sequence_appends.reads().ok_or_else(|| {
            "an `appendRange` without the checked `CharSequence.length` and `get`".into()
        })
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
        self.library_member_of(&fact, identity, role, name)
    }
}
