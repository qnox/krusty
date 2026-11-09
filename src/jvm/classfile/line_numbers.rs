//! `LineNumberTable` marking.
//!
//! One method's source positions are recorded as (pc, line) pairs while its bytecode is built. The
//! rules about which mark wins at a given offset live here, beside the table they produce, rather
//! than at the emission sites that call them.

mod mapper;

pub use mapper::LineNumberMapper;

use super::{ClassWriter, CodeBuilder, LvtEntry};

impl ClassWriter {
    /// Position of the implicit void return recorded by declared-function emission. `None` for an
    /// abstract, diverging, value-returning, or otherwise non-fallthrough method.
    pub fn method_implicit_void_return_pc(&self, name: &str, desc: &str) -> Option<u16> {
        let (n, d) = (self.cp.lookup_utf8(name)?, self.cp.lookup_utf8(desc)?);
        self.methods
            .iter()
            .find(|method| method.name == n && method.desc == d)?
            .implicit_void_return_pc
    }

    /// Attach kotlinc-style debug tables to a previously-added method (matched by name+descriptor):
    /// a `LineNumberTable` plus a whole-method `LocalVariableTable`. Interns each local's name and
    /// descriptor here so per-method visitation fixes constant-pool order.
    pub fn set_method_debug(
        &mut self,
        name: &str,
        desc: &str,
        lnt: Option<(u16, u32)>,
        locals: &[(String, String, u16)],
    ) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self
            .methods
            .iter()
            .any(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            return;
        }
        // A body that marked lines keeps exactly those: kotlinc numbers a method only where its
        // codegen marks a node, never with the declaration's line in front of the first mark.
        let (needs_lnt, needs_lvt) = match self
            .methods
            .iter()
            .find(|method| method.name == n && method.desc == d)
        {
            Some(method) => (method.lnt.is_empty(), method.lvt.is_empty()),
            None => return,
        };
        if !needs_lnt && !needs_lvt {
            return;
        }
        let lvt: Vec<LvtEntry> = if needs_lvt {
            locals
                .iter()
                .map(|(name, descriptor, slot)| {
                    (
                        self.cp.utf8(name),
                        self.cp.utf8(descriptor),
                        *slot,
                        None,
                        None,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let latest = self
            .methods
            .last()
            .is_some_and(|method| method.name == n && method.desc == d);
        if needs_lvt && latest {
            // The local names belong to the method just added, as kotlinc's writer interns them
            // with its body. A later method's rewrite must not lay its own entries out ahead of
            // them: a top-level setter's `<set-?>` stays between the setter and the next method.
            self.include_latest_method_pool();
        }
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d)
        {
            if needs_lnt {
                method.lnt = lnt
                    .map(|(pc, line)| (pc, line as u16))
                    .into_iter()
                    .collect();
            }
            if needs_lvt {
                method.lvt = lvt;
            }
        }
    }

    /// Intern an `init` block's local names before the constructor's parameter rows, so a kept
    /// local's strings precede `this` the way kotlinc visits them.
    pub fn reserve_ranged_local_names(&mut self, locals: &[(u16, u16, u16, String, String)]) {
        for (_, _, _, name, descriptor) in locals {
            self.cp.utf8(name);
            self.cp.utf8(descriptor);
        }
    }

    /// Insert constructor-body locals ahead of the parameter rows [`Self::set_method_debug`] wrote.
    ///
    /// kotlinc visits an `init` block's locals before `this` and the constructor parameters. A
    /// zero-length range stays until the method is written: the store in front of it belongs to
    /// that local, and an unused local then disappears from the table.
    pub fn prepend_ranged_locals(
        &mut self,
        name: &str,
        desc: &str,
        locals: &[(u16, u16, u16, String, String)],
    ) {
        if locals.is_empty() {
            return;
        }
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        let ranged = locals
            .iter()
            .map(|(start, length, slot, local_name, local_desc)| {
                (
                    self.cp.utf8(local_name),
                    self.cp.utf8(local_desc),
                    *slot,
                    Some(*start),
                    Some(*length),
                )
            })
            .collect::<Vec<_>>();
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            let mut combined = ranged;
            combined.append(&mut method.lvt);
            method.lvt = combined;
        }
    }

    /// Replace a method's `LineNumberTable` with exact `(start_pc, line)` entries. Lookup-only:
    /// describing a missing method does not perturb the constant pool.
    pub fn set_method_lines(&mut self, name: &str, desc: &str, entries: &[(u16, u32)]) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        let lineless = |(name_, desc_): &(String, String)| name_ == name && desc_ == desc;
        if self.lineless_methods.iter().any(lineless) {
            return;
        }
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            method.lnt = entries
                .iter()
                .map(|&(pc, line)| (pc, line as u16))
                .collect();
        }
    }

    /// Write no `LineNumberTable` for `name desc`, whatever lines its emission collects: kotlinc
    /// writes none for a `<clinit>` that fills `$$delegatedProperties`.
    pub fn omit_method_lines(&mut self, name: &str, desc: &str) {
        self.lineless_methods
            .push((name.to_string(), desc.to_string()));
    }
}

impl CodeBuilder {
    /// kotlinc's `markLineNumber` at the current offset: an entry when the line differs from the
    /// one the method's [`LineNumberMapper`] last marked.
    ///
    /// Code dropped as dead still advances the mapper, as kotlinc's codegen marks it before its
    /// dead-code pass removes the entry.
    pub fn mark_line(&mut self, line: u32) {
        let line = line.min(u32::from(u16::MAX)) as u16;
        let forgotten = self.lines.last_line().is_none();
        let previous = self.line_marks.last().map(|&(_, previous)| previous);
        if self.lines.mark(line)
            && self.push_line_entry(line)
            && forgotten
            && previous == Some(line)
        {
            // The caller's line written again after an inlined body is that body's restoration,
            // which a constructor's debug-table curation keeps beside an equal entry.
            let pc = self.bytes.len() as u16;
            self.inlined_line_marks.push((pc, line));
        }
    }

    /// An entry for `line` at the current offset without consulting the mapper: a line the
    /// inliner copies with an inline body, or the caller's line kotlinc registers again after an
    /// inlined call inside a condition (`markLineNumberAfterInlineIfNeeded`). The line the mapper
    /// last marked is unchanged.
    pub fn inlined_line(&mut self, line: u16) {
        if self.push_line_entry(line) {
            let pc = self.bytes.len() as u16;
            self.inlined_line_marks.push((pc, line));
        }
    }

    fn push_line_entry(&mut self, line: u16) -> bool {
        if self.dead || self.bytes.len() > u16::MAX as usize {
            return false;
        }
        self.line_marks.push((self.bytes.len() as u16, line));
        true
    }

    /// The method's line mapper, for the node-kind line rule the JVM backend states.
    pub fn line_mapper(&mut self) -> &mut LineNumberMapper {
        &mut self.lines
    }

    /// How many line entries are recorded so far: where an in-place call's own entry goes (see
    /// [`Self::mark_in_place_call_line`]).
    pub fn line_entry_count(&self) -> usize {
        self.line_marks.len()
    }

    /// kotlinc's mark of an `@InlineOnly` call whose arguments are evaluated in place, made once
    /// its arguments have marked theirs: the entry, when due, goes ahead of those arguments' own
    /// entries at `pc`, where `InplaceArgumentsMethodTransformer` leaves the call's line label.
    pub fn mark_in_place_call_line(&mut self, index: usize, pc: u16, line: u32) {
        let line = line.min(u32::from(u16::MAX)) as u16;
        if !self.lines.mark(line) || self.dead || self.bytes.len() > u16::MAX as usize {
            return;
        }
        let index = index.min(self.line_marks.len());
        self.line_marks.insert(index, (pc, line));
    }

    /// kotlinc's `markLineNumberAfterInlineIfNeeded` once an inlined body ends: inside a
    /// condition the caller's line is registered again at once; elsewhere it is forgotten, so the
    /// next mark of any line is written.
    pub fn line_after_inline(&mut self, register: bool) {
        if let Some(line) = self.lines.after_inline(register) {
            self.inlined_line(line);
        }
    }

    /// Record `line` at an offset BEHIND the current one — a spliced body's own line marks, whose
    /// positions the splice fixes and which are added once its bytes are already in place.
    ///
    /// The table must stay ascending by pc, so a mark that would land before the last one already
    /// recorded is refused rather than silently reordering it.
    pub fn add_line_mark_at(&mut self, pc: u16, line: u16) {
        // A `LineNumberTable` `start_pc` must index the code array (JVMS 4.7.12). A spliced body
        // whose bytes were dropped as unreachable leaves its recorded positions past the end, and an
        // entry there makes the whole class unloadable.
        if self.bytes.len() > u16::MAX as usize || pc as usize >= self.bytes.len() {
            return;
        }
        match self.line_marks.last() {
            Some((last_pc, _)) if *last_pc > pc => {}
            Some((last_pc, last_line)) if *last_pc == pc => {
                if *last_line != line {
                    let entry = self.line_marks.last_mut().expect("just observed");
                    entry.1 = line;
                }
            }
            // The line already in effect needs no entry of its own.
            Some((_, last_line)) if *last_line == line => {}
            _ => self.line_marks.push((pc, line)),
        }
    }

    /// Forget which line is in effect, so the next mark is written even for the same line: kotlinc
    /// resets its last line number after an inlined call (`markLineNumberAfterInlineIfNeeded`),
    /// whose code ran under the callee's lines rather than the caller's.
    pub fn forget_line(&mut self) {
        let _ = self.lines.after_inline(false);
    }

    /// Line marks whose provenance is an inlined body or its caller-line restoration.
    pub fn inlined_line_marks(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.inlined_line_marks
            .iter()
            .copied()
            .filter(|mark| self.line_marks.contains(mark))
    }

    /// Whether the mapper holds no line, after an inlined body ([`Self::forget_line`]) or before
    /// the first mark, so the next mark of any line is written.
    pub fn line_forgotten(&self) -> bool {
        self.lines.last_line().is_none()
    }

    /// The line kotlinc's mapper last marked (`lastLineNumber`), if any.
    pub fn current_line(&self) -> Option<u16> {
        self.lines.last_line()
    }

    /// Whether the last entry numbers `line` on a `nop` written just behind the cursor, the
    /// provisional anchor that closes a spliced lambda's frame.
    pub fn line_anchored_by_trailing_nop(&self, line: u16) -> bool {
        self.line_marks.last().is_some_and(|&(pc, marked)| {
            marked == line
                && usize::from(pc) + 1 == self.bytes.len()
                && self.bytes.last() == Some(&0x00)
        })
    }

    /// The recorded `LineNumberTable` marks (empty for a body emitted without line info).
    pub fn line_marks(&self) -> &[(u16, u16)] {
        &self.line_marks
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ClassWriter, CodeBuilder, ACC_PUBLIC, ACC_STATIC};

    #[test]
    fn a_line_mark_can_identify_its_trailing_nop_anchor() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(4);
        code.nop();
        assert!(code.line_anchored_by_trailing_nop(4));
        assert!(!code.line_anchored_by_trailing_nop(5));
        code.dup();
        assert!(!code.line_anchored_by_trailing_nop(4));
    }

    #[test]
    fn marks_at_one_offset_are_all_appended() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(3);
        code.mark_line(4);
        code.aconst_null();
        assert_eq!(code.line_marks(), &[(0, 3), (0, 4)]);
    }

    #[test]
    fn the_line_marked_last_writes_no_second_entry_until_it_is_forgotten() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(3);
        code.aconst_null();
        code.mark_line(3);
        assert_eq!(code.line_marks(), &[(0, 3)]);
        code.forget_line();
        code.mark_line(3);
        code.pop();
        assert_eq!(code.line_marks(), &[(0, 3), (1, 3)]);
    }

    #[test]
    fn an_in_place_call_entry_goes_ahead_of_its_arguments_entries() {
        let mut code = CodeBuilder::new(0);
        let index = code.line_entry_count();
        code.mark_line(5);
        code.aconst_null();
        code.mark_in_place_call_line(index, 0, 6);
        assert_eq!(code.line_marks(), &[(0, 6), (0, 5)]);
    }

    #[test]
    fn implicit_void_return_provenance_is_not_inferred_from_the_last_opcode() {
        let mut writer = ClassWriter::new("FooKt", "java/lang/Object");

        let mut implicit = CodeBuilder::new(0);
        implicit.implicit_ret_void();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "implicit", "()V", &implicit);
        assert_eq!(
            writer.method_implicit_void_return_pc("implicit", "()V"),
            Some(0)
        );

        let mut explicit = CodeBuilder::new(0);
        explicit.ret_void();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "explicit", "()V", &explicit);
        assert_eq!(
            writer.method_implicit_void_return_pc("explicit", "()V"),
            None
        );

        let mut diverging = CodeBuilder::new(0);
        diverging.aconst_null();
        diverging.athrow();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "diverging", "()V", &diverging);
        assert_eq!(
            writer.method_implicit_void_return_pc("diverging", "()V"),
            None
        );
    }
}
