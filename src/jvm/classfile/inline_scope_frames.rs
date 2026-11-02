//! The names an expanded inline frame's locals take when the method numbers its inline scopes
//! (`-Xuse-inline-scopes-numbers`).
//!
//! An inline body expanded in IR records its locals against the frame identities its markers open
//! (`$i$f$…`, `$i$a$…`), named as kotlinc names them without numbers. kotlinc numbers an inlining
//! as `MethodInliner` places it: first the callee's own scopes in the order they start, at the
//! call's line, then each lambda at each `invoke` in the callee's order. A placed lambda's body is
//! a method of its own, numbered by a generator of its own (the scopes it inlines included), and
//! placing it renumbers that body after the scopes already numbered (`FromScopeNumbers`).
//!
//! The emitter numbers each frame within its generator as the frame closes, because the count of
//! scopes the method has numbered so far is shared with the bytecode inliner. The names themselves
//! are only spelled once the table is complete: a frame's operands are recorded after its marker.

use std::borrow::Cow;

use super::local_variables::LocalEntryPlacement;
use super::{ClassWriter, CodeBuilder};
use crate::jvm::inliner::{inline_scopes, InlineScopes};

pub(super) type LocalEntry = (u16, Option<u16>, u16, String, String);

/// One inline frame the method's body expands.
#[derive(Clone, Debug)]
pub(crate) struct InlineScopeFrame {
    pub id: u64,
    pub kind: InlineScopeFrameKind,
    /// The frame that was innermost where this one opened.
    pub parent: Option<u64>,
    /// The placed lambda whose body this frame is part of, `None` for the method's own body.
    pub unit: Option<u64>,
    /// The line in effect where it opened, in the method's lines: the call's own line for an
    /// inlining and an `@InlineOnly` callee's lambda, and the `invoke`'s for any other lambda.
    pub call_site_line: Option<u16>,
    /// Its number in its unit's numbering, `0` until its inlining is numbered.
    pub scope: i32,
    /// For a placed lambda: how many scopes its body numbers, its own included.
    pub count: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineScopeFrameKind {
    Function,
    /// A lambda body. `depth` counts the expansions it was cloned into; a lambda the inlining
    /// places (`placed`) is the caller's own code at its unit's depth, any other is the callee's.
    Lambda {
        depth: u32,
        placed: bool,
    },
}

impl InlineScopeFrame {
    pub(crate) fn is_placed(&self) -> bool {
        matches!(self.kind, InlineScopeFrameKind::Lambda { placed: true, .. })
    }
}

impl CodeBuilder {
    pub(crate) fn inline_scope_frames(&self) -> &[InlineScopeFrame] {
        &self.inline_scope_frames
    }

    pub(crate) fn register_inline_scope_frame(&mut self, frame: InlineScopeFrame) {
        self.inline_scope_frames.push(frame);
    }

    pub(crate) fn inline_scope_frame_mut(&mut self, id: u64) -> Option<&mut InlineScopeFrame> {
        self.inline_scope_frames
            .iter_mut()
            .find(|frame| frame.id == id)
    }

    /// Where the marker of `frame` starts, once it is recorded.
    pub(crate) fn inline_frame_start(&self, frame: u64) -> Option<u16> {
        self.local_entries
            .iter()
            .zip(&self.local_entry_placements)
            .find(|(_, placement)| {
                matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame)
            })
            .map(|(entry, _)| entry.0)
    }

    /// The recorded locals, named as the method's table names them.
    pub fn named_local_entries(&self) -> Cow<'_, [LocalEntry]> {
        if self.inline_scope_frames.is_empty() {
            return Cow::Borrowed(&self.local_entries);
        }
        let names = Naming::new(self).names();
        Cow::Owned(
            self.local_entries
                .iter()
                .zip(names)
                .map(|(entry, name)| (entry.0, entry.1, entry.2, name, entry.4.clone()))
                .collect(),
        )
    }
}

impl ClassWriter {
    /// The method's locals as its table writes them. An inline marker records its call site in
    /// the method's own lines, which kotlinc copies into the class's lines with the method's line
    /// table (`SourceMapCopyingMethodVisitor`); the class map is the one the earlier methods'
    /// copies leave.
    pub(super) fn written_local_entries<'c>(&self, code: &'c CodeBuilder) -> Cow<'c, [LocalEntry]> {
        self.local_entries_written_after(code, &self.method_source_map, &self.methods)
    }

    /// [`Self::written_local_entries`] for a method whose source map `map` follows `earlier`.
    pub(super) fn local_entries_written_after<'c>(
        &self,
        code: &'c CodeBuilder,
        map: &crate::jvm::source_map::SourceMap,
        earlier: &[super::MethodInfo],
    ) -> Cow<'c, [LocalEntry]> {
        let entries = code.named_local_entries();
        if map.is_empty() {
            return entries;
        }
        let calls: Vec<(usize, u16)> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| inline_scopes::is_marker(&entry.3))
            .filter_map(|(index, entry)| {
                let line = inline_scopes::call_site_line(&entry.3)?;
                Some((index, u16::try_from(line).ok()?))
            })
            .collect();
        if calls.is_empty() {
            return entries;
        }
        let mut class = self.source_map.clone();
        for method in earlier {
            if let Some(map) = &method.source_map {
                let lines: Vec<u16> = method.lnt.iter().map(|&(_, line)| line).collect();
                class.copied_lines(map, &lines, &[]);
            }
        }
        let written: Vec<u16> = code.line_marks().iter().map(|&(_, line)| line).collect();
        let queries: Vec<u16> = calls.iter().map(|&(_, line)| line).collect();
        let mapped = class.copied_lines(map, &written, &queries);
        let mut entries = entries.into_owned();
        for ((index, _), line) in calls.into_iter().zip(mapped) {
            entries[index].3 =
                inline_scopes::with_call_site_line(&entries[index].3, |_| i32::from(line));
        }
        Cow::Owned(entries)
    }
}

/// The spelling pass over a complete table.
struct Naming<'c> {
    code: &'c CodeBuilder,
    names: Vec<String>,
    /// Each frame's marker entry.
    markers: Vec<Option<usize>>,
}

impl<'c> Naming<'c> {
    fn new(code: &'c CodeBuilder) -> Self {
        let markers = code
            .inline_scope_frames
            .iter()
            .map(|frame| {
                code.local_entry_placements.iter().position(|placement| {
                    matches!(placement, LocalEntryPlacement::FrameMarker(owner) if *owner == frame.id)
                })
            })
            .collect();
        Naming {
            code,
            names: code
                .local_entries
                .iter()
                .map(|entry| entry.3.clone())
                .collect(),
            markers,
        }
    }

    fn frames(&self) -> &'c [InlineScopeFrame] {
        &self.code.inline_scope_frames
    }

    fn frame(&self, id: u64) -> Option<usize> {
        self.frames().iter().position(|frame| frame.id == id)
    }

    fn names(mut self) -> Vec<String> {
        self.spell_unit(None);
        self.names
    }

    /// Spell every frame of `unit`'s body: the lambdas its inlinings placed, each spelled in its
    /// own body first, then the callee scopes of its inlinings.
    fn spell_unit(&mut self, unit: Option<u64>) {
        let frames = self.frames();
        for (index, frame) in frames.iter().enumerate() {
            if frame.unit == unit && frame.is_placed() {
                self.spell_unit(Some(frame.id));
                self.place(index);
            }
        }
        for (index, frame) in frames.iter().enumerate() {
            if frame.unit == unit && !frame.is_placed() && frame.scope > 0 {
                self.spell_callee_scope(index, unit);
            }
        }
    }

    /// The table entries `frame` owns.
    fn owned(&self, frame: u64) -> impl Iterator<Item = usize> + '_ {
        self.code
            .local_entry_placements
            .iter()
            .enumerate()
            .filter(move |(_, placement)| match placement {
                LocalEntryPlacement::FrameLocal(owner)
                | LocalEntryPlacement::FrameBeforeMarker(owner) => *owner == frame,
                _ => false,
            })
            .map(|(entry, _)| entry)
    }

    fn call_site_line(&self, frame: usize) -> Option<i32> {
        self.frames()[frame].call_site_line.map(i32::from)
    }

    /// A callee scope of an inlining (`addInlineScopesInfoFromIVSuffixes` over the callee's own
    /// table): its marker records its call site, a lambda's marker the scope it was declared in.
    fn spell_callee_scope(&mut self, frame: usize, unit: Option<u64>) {
        let frames = self.frames();
        let this = &frames[frame];
        let Some(marker) = self.markers[frame] else {
            return;
        };
        let surrounding = match this.kind {
            InlineScopeFrameKind::Function => None,
            InlineScopeFrameKind::Lambda { .. } => Some(self.surrounding_scope(frame, unit)),
        };
        let line = self.call_site_line(frame).unwrap_or(match this.kind {
            InlineScopeFrameKind::Function => -1,
            InlineScopeFrameKind::Lambda { .. } => 0,
        });
        self.names[marker] = inline_scopes::numbered_name(
            &self.code.local_entries[marker].3,
            this.scope,
            Some(line),
            surrounding,
        );
        let owned: Vec<usize> = self.owned(this.id).collect();
        for entry in owned {
            self.names[entry] = inline_scopes::numbered_name(
                &self.code.local_entries[entry].3,
                this.scope,
                None,
                None,
            );
        }
    }

    /// `computeSurroundingScopeNumber` for a lambda of the callee: the nearest enclosing callee
    /// scope at the lambda's own depth in the callee, `0` for none.
    fn surrounding_scope(&self, frame: usize, unit: Option<u64>) -> i32 {
        let frames = self.frames();
        let depth = self.nesting(frame, unit);
        let mut next = frames[frame].parent.and_then(|parent| self.frame(parent));
        while let Some(ancestor) = next {
            if Some(frames[ancestor].id) == unit || frames[ancestor].is_placed() {
                break;
            }
            if self.nesting(ancestor, unit) == depth {
                return frames[ancestor].scope;
            }
            next = frames[ancestor]
                .parent
                .and_then(|parent| self.frame(parent));
        }
        0
    }

    /// The scope's depth in its callee's table: an inlined function's is its enclosing scope's
    /// plus one, the inlining's own scope being `0`, and a lambda's is the number of expansions
    /// it was cloned into within the callee.
    fn nesting(&self, frame: usize, unit: Option<u64>) -> i32 {
        let frames = self.frames();
        match frames[frame].kind {
            InlineScopeFrameKind::Lambda { depth, .. } => {
                i32::try_from(depth).unwrap_or(i32::MAX) - self.unit_depth(unit) - 1
            }
            InlineScopeFrameKind::Function => {
                let mut nesting = 0;
                let mut next = frames[frame].parent.and_then(|parent| self.frame(parent));
                while let Some(ancestor) = next {
                    if Some(frames[ancestor].id) == unit || frames[ancestor].is_placed() {
                        break;
                    }
                    if frames[ancestor].kind == InlineScopeFrameKind::Function {
                        nesting += 1;
                    }
                    next = frames[ancestor]
                        .parent
                        .and_then(|parent| self.frame(parent));
                }
                nesting
            }
        }
    }

    fn unit_depth(&self, unit: Option<u64>) -> i32 {
        unit.and_then(|unit| self.frame(unit))
            .and_then(|frame| match self.frames()[frame].kind {
                InlineScopeFrameKind::Lambda { depth, .. } => i32::try_from(depth).ok(),
                InlineScopeFrameKind::Function => None,
            })
            .unwrap_or(0)
    }

    /// Place the lambda `frame` (`addInlineScopesInfoFromScopeNumbers` over its body as its own
    /// method numbered it): its scopes follow the ones its unit numbered before it.
    fn place(&mut self, frame: usize) {
        let this = &self.frames()[frame];
        let Some(marker) = self.markers[frame].filter(|_| this.scope > 0) else {
            return;
        };
        let (start, length) = (
            self.code.local_entries[marker].0,
            self.code.local_entries[marker].1,
        );
        let end = length.map_or(u16::MAX, |length| start.saturating_add(length));
        let mut body: Vec<usize> = vec![marker];
        for (entry, placement) in self.code.local_entry_placements.iter().enumerate() {
            if entry == marker {
                continue;
            }
            let inside = match placement {
                LocalEntryPlacement::FrameMarker(owner)
                | LocalEntryPlacement::FrameLocal(owner)
                | LocalEntryPlacement::FrameBeforeMarker(owner) => self.within(*owner, this.id),
                LocalEntryPlacement::Plain => {
                    let at = self.code.local_entries[entry].0;
                    at >= start && at < end
                }
            };
            if inside {
                body.push(entry);
            }
        }
        body.sort_unstable();
        let table: Vec<(u16, u16, String)> = body
            .iter()
            .map(|&entry| {
                let (start, length, ..) = &self.code.local_entries[entry];
                let length = length.unwrap_or_else(|| end.saturating_sub(*start));
                (*start, length, self.names[entry].clone())
            })
            .collect();
        let mut scopes = InlineScopes {
            inlined: this.scope - 1,
            call_site_line: self.call_site_line(frame).unwrap_or(0),
        };
        let numbered = scopes.number_table(&[], &table);
        for (entry, name) in body.into_iter().zip(numbered) {
            self.names[entry] = name;
        }
    }

    /// Whether `frame` is `lambda` or part of its body.
    fn within(&self, frame: u64, lambda: u64) -> bool {
        let mut next = Some(frame);
        while let Some(id) = next {
            if id == lambda {
                return true;
            }
            let Some(index) = self.frame(id) else {
                return false;
            };
            next = self.frames()[index].parent;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(start: u16, length: u16, name: &str) -> LocalEntry {
        (start, Some(length), 0, name.to_string(), "I".to_string())
    }

    fn frame(
        id: u64,
        kind: InlineScopeFrameKind,
        parent: Option<u64>,
        line: u16,
    ) -> InlineScopeFrame {
        InlineScopeFrame {
            id,
            kind,
            parent,
            unit: None,
            call_site_line: Some(line),
            scope: 0,
            count: 0,
        }
    }

    fn names(code: &CodeBuilder) -> Vec<String> {
        code.named_local_entries()
            .iter()
            .map(|entry| entry.3.clone())
            .collect()
    }

    /// `numbers.forEach { n -> sum += n }` at line 5, its `invoke` mapped to line 23: the callee's
    /// scope comes first, then the lambda it placed.
    #[test]
    fn a_placed_lambda_follows_its_callees_scopes() {
        let mut code = CodeBuilder::new(0);
        let lambda = InlineScopeFrameKind::Lambda {
            depth: 0,
            placed: true,
        };
        code.insert_inline_frame_marker(1, true, entry(55, 5, "$i$a$-forEach-K$total$1"));
        code.add_inline_frame_local(1, entry(52, 8, "n"));
        code.add_inline_frame_local(0, entry(42, 19, "element$iv"));
        code.insert_inline_frame_marker(0, false, entry(15, 50, "$i$f$forEach"));
        code.add_inline_frame_local(0, entry(13, 52, "$this$forEach$iv"));
        code.add_local_entry(8, Some(59), 1, "sum", "I");
        let mut callee = frame(0, InlineScopeFrameKind::Function, None, 5);
        callee.scope = 1;
        let mut placed = frame(1, lambda, Some(0), 23);
        placed.scope = 2;
        placed.count = 1;
        code.register_inline_scope_frame(callee);
        code.register_inline_scope_frame(placed);
        assert_eq!(
            names(&code),
            [
                "$i$a$-forEach-K$total$1\\2\\23\\0",
                "n\\2",
                "element\\1",
                "$i$f$forEach\\1\\5",
                "$this$forEach\\1",
                "sum",
            ]
        );
    }

    /// A lambda placed inside a placed lambda's body is numbered within that body, and placing the
    /// outer lambda renumbers both after the scopes before it.
    #[test]
    fn a_placed_body_is_renumbered_where_it_is_placed() {
        let mut code = CodeBuilder::new(0);
        let lambda = InlineScopeFrameKind::Lambda {
            depth: 0,
            placed: true,
        };
        code.insert_inline_frame_marker(2, true, entry(65, 4, "$i$a$-let-K$g$1$1"));
        code.add_inline_frame_local(2, entry(62, 7, "it"));
        code.insert_inline_frame_marker(1, true, entry(55, 23, "$i$a$-forEach-K$g$1"));
        code.add_inline_frame_local(1, entry(52, 26, "n"));
        code.insert_inline_frame_marker(0, false, entry(15, 70, "$i$f$forEach"));
        let mut callee = frame(0, InlineScopeFrameKind::Function, None, 12);
        callee.scope = 1;
        let mut outer = frame(1, lambda, Some(0), 19);
        outer.scope = 2;
        outer.count = 2;
        let mut inner = frame(2, lambda, Some(1), 13);
        inner.unit = Some(1);
        inner.scope = 1;
        inner.count = 1;
        code.register_inline_scope_frame(callee);
        code.register_inline_scope_frame(outer);
        code.register_inline_scope_frame(inner);
        assert_eq!(
            names(&code),
            [
                "$i$a$-let-K$g$1$1\\3\\13\\2",
                "it\\3",
                "$i$a$-forEach-K$g$1\\2\\19\\0",
                "n\\2",
                "$i$f$forEach\\1\\12",
            ]
        );
    }
}
