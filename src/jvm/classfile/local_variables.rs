//! A method's `LocalVariableTable` strings: when its names and descriptors intern.

use super::{ClassWriter, CodeBuilder, LocalEntryPlacement, LvtEntry};

/// Exclusive end of a recorded local range. An open-ended range runs through the method.
fn range_end(start: u16, length: Option<u16>) -> u32 {
    match length {
        Some(length) => u32::from(start).saturating_add(u32::from(length)),
        None => u32::MAX,
    }
}

/// The bytes between a frame local and its function marker are only `iload`/`aload` and the other
/// narrow local loads. The inline epilogue removes that load, so the two ranges end together.
fn gap_is_only_local_loads(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        let opcode = bytes[index];
        let len = match opcode {
            0x15 | 0x16 | 0x17 | 0x18 | 0x19 => 2,
            0x1a..=0x2d => 1,
            _ => return false,
        };
        if index + len > bytes.len() {
            return false;
        }
        index += len;
    }
    !bytes.is_empty()
}

impl CodeBuilder {
    /// Record a local owned by an inline frame. Its marker may close later and insert immediately
    /// ahead of the frame's first already-recorded local.
    pub(crate) fn add_inline_frame_local(
        &mut self,
        frame: u64,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let position = self
            .local_entry_placements
            .iter()
            .position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame)
            })
            .and_then(|marker| {
                self.local_entry_placements[marker + 1..]
                    .iter()
                    .position(|placement| matches!(placement, LocalEntryPlacement::FrameMarker(_)))
                    .map(|next| marker + 1 + next)
            })
            .unwrap_or(self.local_entries.len());
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameLocal(frame));
    }

    /// Record an inline frame marker.
    ///
    /// A lambda marker precedes its already-recorded locals and keeps every nested-frame and catch
    /// row that belongs ahead of them. A function marker follows a local of its frame whose range
    /// ended before the marker — an iteration's `element` — and the catch rows recorded before
    /// that local. A local that ends with the marker stays after `$i$f$`, including one separated
    /// from it only by a load the inline epilogue removes (`use`'s `closed`). Operands still open
    /// are recorded after this call and follow the marker.
    pub(crate) fn insert_inline_frame_marker(
        &mut self,
        frame: u64,
        precede_recorded_locals: bool,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let marker_end = range_end(entry.0, entry.1);
        let position = if precede_recorded_locals {
            let first_local = self.local_entry_placements.iter().position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameLocal(owner) if *owner == frame)
            });
            first_local.unwrap_or_else(|| {
                self.local_entry_placements
                    .iter()
                    .rposition(|placement| {
                        matches!(
                            placement,
                            LocalEntryPlacement::FrameBeforeMarker(owner) if *owner == frame
                        )
                    })
                    .map_or(self.local_entries.len(), |position| position + 1)
            })
        } else {
            self.function_marker_position(frame, marker_end)
        };
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameMarker(frame));
    }

    /// Where a function marker sits among locals of `frame` that are already in the table.
    ///
    /// Locals that ended strictly earlier, and catch rows ahead of the first local that is still
    /// live at the marker's end, stay in front. Locals that share the marker's end stay behind it.
    /// A gap that is only a local load does too: the inline epilogue removes that load, so the
    /// finished ranges end together and the local follows `$i$f$`.
    fn function_marker_position(&self, frame: u64, marker_end: u32) -> usize {
        let stays_with_marker = |local_end: u32| !self.ended_before_marker(local_end, marker_end);
        let first_following = self
            .local_entries
            .iter()
            .zip(self.local_entry_placements.iter())
            .position(|(recorded, placement)| {
                matches!(placement, LocalEntryPlacement::FrameLocal(owner) if *owner == frame)
                    && stays_with_marker(range_end(recorded.0, recorded.1))
            });
        let search_end = first_following.unwrap_or(self.local_entries.len());
        self.local_entries[..search_end]
            .iter()
            .zip(self.local_entry_placements[..search_end].iter())
            .rposition(|(recorded, placement)| match placement {
                LocalEntryPlacement::FrameBeforeMarker(owner) if *owner == frame => true,
                LocalEntryPlacement::FrameLocal(owner) if *owner == frame => {
                    !stays_with_marker(range_end(recorded.0, recorded.1))
                }
                _ => false,
            })
            .map_or(
                first_following.unwrap_or(self.local_entries.len()),
                |position| position + 1,
            )
    }

    /// Whether `local_end` is still strictly before `marker_end` once a trailing load the inline
    /// epilogue deletes is ignored. That load sits in the marker's range only, and the finished
    /// table gives both rows the same end.
    fn ended_before_marker(&self, local_end: u32, marker_end: u32) -> bool {
        if local_end >= marker_end {
            return false;
        }
        let Ok(start) = usize::try_from(local_end) else {
            return true;
        };
        let Ok(end) = usize::try_from(marker_end) else {
            return true;
        };
        if end > self.bytes.len() || start > end {
            return true;
        }
        !gap_is_only_local_loads(&self.bytes[start..end])
    }

    /// Record a catch row before its containing inline frame's marker. Handler emission may close
    /// after that marker's lexical scope; placement must not depend on that timing.
    pub(crate) fn add_inline_frame_catch(
        &mut self,
        frame: u64,
        entry: (u16, Option<u16>, u16, String, String),
    ) {
        let position = self
            .local_entry_placements
            .iter()
            .position(|placement| {
                matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame)
            })
            .unwrap_or(self.local_entries.len());
        self.local_entries.insert(position, entry);
        self.local_entry_placements
            .insert(position, LocalEntryPlacement::FrameBeforeMarker(frame));
    }
}

impl ClassWriter {
    /// The `LocalVariableTable` a method's emission recorded, its strings interned.
    pub(super) fn local_table(&mut self, code: &CodeBuilder) -> Vec<LvtEntry> {
        code.local_entries()
            .iter()
            // A `LocalVariableTable` `start_pc` must index the code array (JVMS §4.7.13) —
            // HotSpot's class-file parser rejects the whole class otherwise. A local DECLARED
            // in a region the emitter dropped as unreachable (`val y: Int = boom() ?: 1`) has
            // its start recorded past the last instruction and describes no live range, so it
            // goes with the code. An empty range stays until the method is written, as in
            // kotlinc (`prepareForEmitting`): the store before it is a named local's.
            .filter(|(start, len, ..)| {
                let start = usize::from(*start);
                let end = len.map_or(code.bytes.len(), |length| start + usize::from(length));
                start < code.bytes.len() && end <= code.bytes.len()
            })
            .map(|(start, len, slot, name, descriptor)| {
                (
                    self.cp.utf8(name),
                    self.cp.utf8(descriptor),
                    *slot,
                    Some(*start),
                    *len,
                )
            })
            .collect()
    }

    /// Keep the tables a constructor's own emission recorded. `add_method` curates every `<init>`'s
    /// line and local tables from its class's declarations; a declared secondary constructor's are
    /// its body's, as for any method.
    pub fn keep_method_debug(&mut self, name: &str, desc: &str, code: &CodeBuilder) {
        let n = self
            .cp
            .lookup_utf8(name)
            .expect("a just-emitted method retains its interned name");
        let d = self
            .cp
            .lookup_utf8(desc)
            .expect("a just-emitted method retains its interned descriptor");
        let lvt = self.local_table(code);
        let method = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
            .expect("a just-emitted method must exist before retaining its debug tables");
        method.lnt = code.line_marks().to_vec();
        method.lvt = lvt;
    }

    /// Intern a method's `LocalVariableTable` names and descriptors, in table order, once its body
    /// is complete. ASM visits the local variables after the instructions and before `visitMaxs`,
    /// so a nested block's local follows every constant of the method body, not the instruction
    /// that closed its scope, and precedes the frame classes.
    pub(super) fn intern_local_table(&mut self, code: &CodeBuilder) {
        for (_, _, _, local, descriptor) in code.local_entries() {
            self.cp.utf8(local);
            self.cp.utf8(descriptor);
        }
    }

    /// Intern a method's `LocalVariableTable` names and descriptors BEFORE its code is added.
    ///
    /// ASM visits `visitLocalVariable` before `visitMaxs`, so kotlinc's pool carries a local's name
    /// and descriptor ahead of every class constant the frame computation introduces. krusty builds
    /// the `StackMapTable` inside `add_method`, which interns each parameter's verification type —
    /// so without this reservation a parameter whose class appears NOWHERE else in the class file
    /// (the serialization constructor's marker) lands ahead of the local names instead of behind
    /// them.
    pub fn reserve_method_lvt(&mut self, locals: &[(String, String, u16)]) {
        for (name, descriptor, _) in locals {
            self.cp.utf8(name);
            self.cp.utf8(descriptor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::CodeBuilder;

    const FRAME: u64 = 1;

    fn local(start: u16, length: u16, name: &str) -> (u16, Option<u16>, u16, String, String) {
        (start, Some(length), 0, name.to_string(), "I".to_string())
    }

    fn names(code: &CodeBuilder) -> Vec<&str> {
        code.local_entries()
            .iter()
            .map(|(_, _, _, name, _)| name.as_str())
            .collect()
    }

    #[test]
    fn a_function_marker_follows_a_local_that_ended_before_it() {
        let mut code = CodeBuilder::new(0);
        // forEach: `element` ends at 89; `$i$f$` and the receiver end at 93.
        code.add_inline_frame_local(FRAME, local(68, 21, "element$iv"));
        code.insert_inline_frame_marker(FRAME, false, local(44, 49, "$i$f$forEach"));
        code.add_inline_frame_local(FRAME, local(42, 51, "$this$forEach$iv"));
        assert_eq!(
            names(&code),
            ["element$iv", "$i$f$forEach", "$this$forEach$iv"]
        );
    }

    #[test]
    fn a_function_marker_precedes_a_local_that_ends_with_it() {
        let mut code = CodeBuilder::new(0);
        // use: the catch ends at 84; `closed`, `$i$f$`, and the receiver all end at 108.
        code.add_inline_frame_catch(FRAME, local(58, 26, "e$iv"));
        code.add_inline_frame_local(FRAME, local(19, 89, "closed$iv"));
        code.insert_inline_frame_marker(FRAME, false, local(16, 92, "$i$f$consumeAndClose"));
        code.add_inline_frame_local(FRAME, local(14, 94, "$this$consumeAndClose$iv"));
        assert_eq!(
            names(&code),
            [
                "e$iv",
                "$i$f$consumeAndClose",
                "closed$iv",
                "$this$consumeAndClose$iv",
            ]
        );
    }

    #[test]
    fn a_function_marker_precedes_a_local_separated_only_by_a_load() {
        let mut code = CodeBuilder::new(0);
        code.nop();
        code.add_inline_frame_local(FRAME, local(0, 1, "closed$iv"));
        code.aload(5);
        code.insert_inline_frame_marker(FRAME, false, local(0, 3, "$i$f$consumeAndClose"));
        assert_eq!(names(&code), ["$i$f$consumeAndClose", "closed$iv"]);
    }

    #[test]
    fn a_lambda_marker_stays_ahead_of_body_locals_that_end_with_it() {
        let mut code = CodeBuilder::new(0);
        code.add_inline_frame_local(FRAME, local(26, 8, "r"));
        code.insert_inline_frame_marker(FRAME, true, local(29, 5, "$i$a$-consumeAndClose"));
        assert_eq!(names(&code), ["$i$a$-consumeAndClose", "r"]);
    }
}
