//! `StringBuilder.append(CharSequence?)` and `append(CharSequence?, Int, Int)` on the runtime's
//! builder, as each target's stdlib declares them. The ranged append's range check is the
//! caller's: it throws Kotlin exceptions, which need the body's exception classes.
//!
//! Kotlin/Wasm JS appends the sequence's `toString()`. Kotlin/Wasm WASI copies its units instead:
//! `append` reads `value.length`, and `appendRange` checks the range against a second read and
//! copies a `String` whole or every other sequence one indexed `Char` at a time. A custom sequence's
//! `length` and `get` therefore run on WASI and its `toString` does not. Those two reads are the
//! checked `CharSequence` members the handoff froze
//! (`krusty::backend::CheckedBackendCallables::char_sequence_protocol`), called through their
//! interface stubs, which a string, the builder and every program class implementing them answer.

use super::encode::{BlockType, Code, Function, HeapType, Module, ValType};
use super::interface_dispatch::MemberRef;
use super::objects::REFERENCE;
use super::runtime::{Runtime, ADD32, GE_S32, GT_S32, LT_S32, OR32, SUB32};

/// The append of a `CharSequence` to the runtime's builder, as the target declares it.
pub(super) struct CharSequenceAppends {
    /// The target copies an appended sequence's indexed units (WASI) instead of rendering it.
    by_index: bool,
    /// The checked `CharSequence` members a copy reads through.
    reads: Option<SequenceReads>,
    /// `(builder, value: CharSequence?) -> builder`, once something appends a sequence.
    append: Option<u32>,
    /// `(builder, value: CharSequence?, start, end) -> builder`, once something appends a range.
    append_range: Option<u32>,
}

/// The checked `CharSequence.length` and `CharSequence.get(Int)` members.
#[derive(Clone, Copy)]
pub(super) struct SequenceReads {
    pub(super) length: MemberRef,
    pub(super) get: MemberRef,
}

impl CharSequenceAppends {
    pub(super) fn new(by_index: bool) -> Self {
        Self {
            by_index,
            reads: None,
            append: None,
            append_range: None,
        }
    }

    /// Whether an append reads the sequence through its checked members.
    pub(super) fn reads_by_index(&self) -> bool {
        self.by_index
    }

    /// Whether the members an append reads through are recorded.
    pub(super) fn has_reads(&self) -> bool {
        self.reads.is_some()
    }

    pub(super) fn reads(&self) -> Option<SequenceReads> {
        self.reads
    }

    pub(super) fn record_reads(&mut self, reads: SequenceReads) {
        self.reads.get_or_insert(reads);
    }
}

const MISSING_READS: &str =
    "an append of a `CharSequence` without its checked `length` and `get` declarations";

impl Runtime {
    /// `(builder, value: CharSequence?) -> builder`: the target's `StringBuilder.append` of a
    /// nullable `CharSequence`, `"null"` for `null`.
    pub(super) fn append_char_sequence(&mut self, module: &mut Module) -> Result<u32, String> {
        if let Some(append) = self.char_sequence_appends.append {
            return Ok(append);
        }
        let mut code = Code::default();
        let mut locals = Vec::new();
        let (builder, value) = (0, 1);
        let builders = &self.string_builders;
        if self.char_sequence_appends.by_index {
            let reads = self.char_sequence_appends.reads.ok_or(MISSING_READS)?;
            let length = self.objects.interfaces.stub(module, reads.length);
            let get = self.objects.interfaces.stub(module, reads.get);
            let string = HeapType::Concrete(self.string);
            let (end, index) = (2, 3);
            locals.extend([ValType::I32, ValType::I32]);
            // `value ?: "null"`, appended as the string it is.
            code.local_get(value)
                .ref_is_null()
                .if_(BlockType::Empty)
                .local_get(builder)
                .ref_null(string)
                .call(builders.append_string)
                .return_()
                .end();
            // `append` reads the length, and `appendRange` checks `0..end` against a second read;
            // only a sequence whose length changed between them fails the check.
            code.local_get(value)
                .call(length)
                .local_tee(end)
                .local_get(value)
                .call(length)
                .op(GT_S32)
                .local_get(end)
                .i32_const(0)
                .op(LT_S32)
                .op(OR32)
                .if_(BlockType::Empty)
                .unreachable()
                .end();
            // A string is copied whole, as `insertString` does.
            code.local_get(value)
                .ref_test(false, string)
                .if_(BlockType::Empty)
                .local_get(builder)
                .local_get(value)
                .ref_cast(false, string)
                .call(builders.append_string)
                .return_()
                .end();
            // Every other sequence is copied one `value[index]` at a time.
            code.block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(index)
                .local_get(end)
                .op(GE_S32)
                .br_if(1)
                .local_get(builder)
                .local_get(value)
                .local_get(index)
                .call(get)
                .call(builders.append_char)
                .drop_()
                .local_get(index)
                .i32_const(1)
                .op(ADD32)
                .local_set(index)
                .br(0)
                .end()
                .end()
                .local_get(builder);
        } else {
            code.local_get(builder)
                .local_get(value)
                .call(self.object_strings.any_to_string)
                .call(builders.append_string);
        }
        let ty = module.func_type(vec![REFERENCE, REFERENCE], vec![REFERENCE]);
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.char_sequence_appends.append = Some(function);
        Ok(function)
    }

    /// `(builder, value: CharSequence?, start, end) -> builder`: the target's
    /// `StringBuilder.append(CharSequence?, Int, Int)` after its range check, which appends
    /// `value[start, end)`, or that range of `"null"` for `null`; JS clamps the range to the
    /// rendered string, as its `substring` does. The caller has checked
    /// `0 <= start <= end <= value.length` and thrown for a range outside it
    /// (`Body::check_char_sequence_range`), so nothing is written for an invalid one.
    pub(super) fn append_char_sequence_range(
        &mut self,
        module: &mut Module,
    ) -> Result<u32, String> {
        if let Some(append) = self.char_sequence_appends.append_range {
            return Ok(append);
        }
        let mut code = Code::default();
        let mut locals = Vec::new();
        let (builder, value, start, end) = (0, 1, 2, 3);
        let builders = &self.string_builders;
        if self.char_sequence_appends.by_index {
            let reads = self.char_sequence_appends.reads.ok_or(MISSING_READS)?;
            let get = self.objects.interfaces.stub(module, reads.get);
            let string = HeapType::Concrete(self.string);
            let (index, at, units) = (4, 5, 6);
            locals.extend([ValType::I32, ValType::I32, ValType::reference(self.chars)]);
            // `value ?: "null"`, the range of the string it is.
            code.local_get(value)
                .ref_is_null()
                .if_(BlockType::Empty)
                .local_get(builder)
                .ref_null(string)
                .local_get(start)
                .local_get(end)
                .call(builders.append_range)
                .return_()
                .end();
            // A string is copied whole, as `insertString` does.
            code.local_get(value)
                .ref_test(false, string)
                .if_(BlockType::Empty)
                .local_get(builder)
                .local_get(value)
                .ref_cast(false, string)
                .local_get(start)
                .local_get(end)
                .call(builders.append_range)
                .return_()
                .end();
            // Every other sequence is copied as `array[_length++] = value[index]`: the backing
            // array and the unit's offset are taken, and the unit counted in use, before the
            // sequence's getter runs. A getter that reads this builder sees the unit, one that
            // throws leaves it counted, and one that grows the builder does not redirect the
            // store, which goes to the array taken first.
            // The storage for the whole range is prepared once, before the first getter runs, as
            // `appendRange` ensures capacity for `end - start`.
            code.local_get(builder)
                .local_get(end)
                .local_get(start)
                .op(SUB32)
                .call(builders.ensure_capacity)
                .drop_();
            code.local_get(start).local_set(index);
            code.block(BlockType::Empty)
                .loop_(BlockType::Empty)
                .local_get(index)
                .local_get(end)
                .op(GE_S32)
                .br_if(1)
                .local_get(builder)
                .call(builders.claim_unit)
                .local_set(at)
                .local_get(builder)
                .call(builders.units)
                .local_set(units)
                .local_get(units)
                .local_get(at)
                .local_get(value)
                .local_get(index)
                .call(get)
                .call(builders.set_unit)
                .local_get(index)
                .i32_const(1)
                .op(ADD32)
                .local_set(index)
                .br(0)
                .end()
                .end()
                .local_get(builder);
        } else {
            code.local_get(builder)
                .local_get(value)
                .call(self.object_strings.any_to_string)
                .local_get(start)
                .local_get(end)
                .call(builders.append_clamped_range);
        }
        let ty = module.func_type(
            vec![REFERENCE, REFERENCE, ValType::I32, ValType::I32],
            vec![REFERENCE],
        );
        let function = module.declare();
        module.define(function, Function { ty, locals, code });
        self.char_sequence_appends.append_range = Some(function);
        Ok(function)
    }

    /// The stub of the checked `CharSequence.length` a ranged append reads before it renders or
    /// copies, on every target: recorded by `record_char_sequence_reads`.
    pub(super) fn char_sequence_range_length(&mut self, module: &mut Module) -> Option<u32> {
        let reads = self.char_sequence_appends.reads?;
        Some(self.objects.interfaces.stub(module, reads.length))
    }

    /// The stub of the checked `CharSequence.length` an append reads, on a target that reads it.
    pub(super) fn char_sequence_length(&mut self, module: &mut Module) -> Option<u32> {
        if !self.char_sequence_appends.by_index {
            return None;
        }
        let reads = self.char_sequence_appends.reads?;
        Some(self.objects.interfaces.stub(module, reads.length))
    }
}
