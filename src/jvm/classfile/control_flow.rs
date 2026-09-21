//! Label ownership, branches, and offset relocation for a method's bytecode stream.

use super::{CodeBuilder, Label};

impl CodeBuilder {
    /// `nop`: occupies an offset and touches nothing. Its use is the caller's business — an
    /// exception table's bounds are one, since a range delimited by a real instruction of its own
    /// does not depend on what happens to sit next to it.
    pub fn nop(&mut self) {
        self.op(0x00, 0);
    }

    pub(crate) fn is_dead(&self) -> bool {
        self.dead
    }

    pub fn new_label(&mut self) -> Label {
        let id = self.labels.len() as u32;
        self.labels.push(usize::MAX);
        self.dead_bound.push(false);
        Label {
            builder: self.id,
            index: id,
        }
    }

    /// Bind `l` here. Inside a dropped dead region this revives emission only if control can actually
    /// arrive: some branch to `l` was already emitted. Switch destinations are arrivals too.
    pub fn bind(&mut self, l: Label) {
        let index = self.label_index(l);
        self.labels[index] = self.bytes.len();
        if self.dead {
            let branched_here = self.fixups.iter().any(|&(_, target)| target == l)
                || self.switch_fixups.iter().any(|&(_, _, target)| target == l);
            if branched_here {
                self.dead = false;
            } else {
                self.dead_bound[index] = true;
            }
        }
    }

    /// Bind `l` at an explicit offset that some ALREADY-EMITTED branch targets, whatever the
    /// stream's reachability is now.
    ///
    /// [`Self::bind_at`] declines while the stream is dead, which is right for a label bound where
    /// emission has stopped. A coroutine machine's states are different: their positions are found
    /// after the whole body is emitted — so the stream has just ended in a `return` — and the
    /// dispatch that reaches them was emitted long before.
    pub fn bind_target_at(&mut self, l: Label, offset: usize) {
        let index = self.label_index(l);
        self.labels[index] = offset;
        self.dead_bound[index] = false;
    }

    /// Bind `l` as the target of a branch this builder cannot see.
    ///
    /// A coroutine machine's resume state is reached only from the dispatch in the enclosing method,
    /// and is emitted right after the `areturn` that leaves the frame on suspension. That return
    /// makes the stream dead, so the state — reachable in the finished method — would be dropped as
    /// unreachable here. Binding it this way says the arrival exists even though no branch to it has
    /// been emitted.
    pub fn bind_external_target(&mut self, l: Label) {
        let index = self.label_index(l);
        self.labels[index] = self.bytes.len();
        self.dead = false;
        self.dead_bound[index] = false;
    }

    /// Bind `l` as an exception-handler entry guarding already-bound `[start, end)` ranges.
    pub fn bind_handler(&mut self, l: Label, protects: &[(Label, Label)]) {
        let index = self.label_index(l);
        self.labels[index] = self.bytes.len();
        let guards_live_code = protects.iter().any(|&(s, e)| {
            let (s_index, e_index) = (self.label_index(s), self.label_index(e));
            let (s_off, e_off) = (self.labels[s_index], self.labels[e_index]);
            s_off != usize::MAX && s_off < e_off && !self.is_dead_bound(s.index)
        });
        if guards_live_code {
            self.dead = false;
        } else if self.dead {
            self.dead_bound[index] = true;
        }
    }

    /// Bind a label at an explicit byte offset inside a live, spliced inline body.
    pub fn bind_at(&mut self, l: Label, offset: usize) {
        if self.dead {
            return;
        }
        let index = self.label_index(l);
        self.labels[index] = offset;
    }

    pub(super) fn branch(&mut self, opcode: u8, l: Label, delta: i32) {
        if self.dead {
            self.adjust(delta);
            return;
        }
        self.bytes.push(opcode);
        let pos = self.bytes.len();
        self.fixups.push((pos, l));
        self.bytes.extend_from_slice(&[0, 0]);
        self.adjust(delta);
    }

    pub fn goto(&mut self, l: Label) {
        self.branch(0xa7, l, 0);
        self.dead = true;
    }

    /// `tableswitch low..=high`, one target per key in range. The table is four-byte aligned from
    /// the code-array start, and each four-byte target offset is measured from the opcode.
    pub fn tableswitch(&mut self, low: i32, high: i32, default: Label, targets: &[Label]) {
        debug_assert_eq!(
            targets.len() as i64,
            i64::from(high) - i64::from(low) + 1,
            "a tableswitch needs one target per key in low..=high"
        );
        if self.dead {
            self.adjust(-1);
            return;
        }
        let opcode = self.bytes.len();
        self.bytes.push(0xaa);
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        self.switch_operand(opcode, default);
        self.bytes.extend_from_slice(&low.to_be_bytes());
        self.bytes.extend_from_slice(&high.to_be_bytes());
        for &target in targets {
            self.switch_operand(opcode, target);
        }
        self.adjust(-1);
        self.dead = true;
    }

    /// `lookupswitch` over explicit keys. The JVM requires ascending key order.
    pub fn lookupswitch(&mut self, default: Label, pairs: &[(i32, Label)]) {
        debug_assert!(
            pairs.windows(2).all(|w| w[0].0 < w[1].0),
            "a lookupswitch needs its keys in ascending order"
        );
        if self.dead {
            self.adjust(-1);
            return;
        }
        let opcode = self.bytes.len();
        self.bytes.push(0xab);
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        self.switch_operand(opcode, default);
        self.bytes
            .extend_from_slice(&(pairs.len() as i32).to_be_bytes());
        for &(key, target) in pairs {
            self.bytes.extend_from_slice(&key.to_be_bytes());
            self.switch_operand(opcode, target);
        }
        self.adjust(-1);
        self.dead = true;
    }

    fn switch_operand(&mut self, opcode: usize, l: Label) {
        self.switch_fixups.push((self.bytes.len(), opcode, l));
        self.bytes.extend_from_slice(&[0, 0, 0, 0]);
    }

    pub fn ifeq(&mut self, l: Label) {
        self.branch(0x99, l, -1);
    }
    pub fn ifne(&mut self, l: Label) {
        self.branch(0x9a, l, -1);
    }
    pub fn if_icmpeq(&mut self, l: Label) {
        self.branch(0x9f, l, -2);
    }
    pub fn if_icmpne(&mut self, l: Label) {
        self.branch(0xa0, l, -2);
    }
    pub fn if_icmplt(&mut self, l: Label) {
        self.branch(0xa1, l, -2);
    }
    pub fn if_icmpge(&mut self, l: Label) {
        self.branch(0xa2, l, -2);
    }
    pub fn if_icmpgt(&mut self, l: Label) {
        self.branch(0xa3, l, -2);
    }
    pub fn if_icmple(&mut self, l: Label) {
        self.branch(0xa4, l, -2);
    }
    pub fn lcmp(&mut self) {
        self.op(0x94, -3);
    }
    pub fn dcmpg(&mut self) {
        self.op(0x98, -3);
    }
    pub fn dcmpl(&mut self) {
        self.op(0x97, -3);
    }
    pub fn ifnull(&mut self, l: Label) {
        self.branch(0xc6, l, -1);
    }
    pub fn ifnonnull(&mut self, l: Label) {
        self.branch(0xc7, l, -1);
    }
    pub fn iflt(&mut self, l: Label) {
        self.branch(0x9b, l, -1);
    }
    pub fn ifge(&mut self, l: Label) {
        self.branch(0x9c, l, -1);
    }
    pub fn ifgt(&mut self, l: Label) {
        self.branch(0x9d, l, -1);
    }
    pub fn ifle(&mut self, l: Label) {
        self.branch(0x9e, l, -1);
    }

    /// Resolve all branch offsets whose destinations belong to this builder.
    pub fn link_local_branches(&mut self) {
        for &(pos, label) in &self.fixups {
            if label.builder != self.id {
                continue;
            }
            let target = self.labels[label.index as usize];
            debug_assert!(target != usize::MAX, "unbound label {}", label.index);
            let off = target as i64 - (pos - 1) as i64;
            let bytes = (off as i16).to_be_bytes();
            self.bytes[pos..pos + 2].copy_from_slice(&bytes);
        }
        for &(pos, opcode, label) in &self.switch_fixups {
            if label.builder != self.id {
                continue;
            }
            let target = self.labels[label.index as usize];
            debug_assert!(target != usize::MAX, "unbound label {}", label.index);
            let off = (target as i64 - opcode as i64) as i32;
            self.bytes[pos..pos + 4].copy_from_slice(&off.to_be_bytes());
        }
    }

    /// Branch operands whose destinations belong to an enclosing bytecode builder.
    pub fn external_branches(&self) -> Vec<(usize, Label)> {
        self.fixups
            .iter()
            .filter(|(_, label)| label.builder != self.id)
            .copied()
            .collect()
    }

    /// Resolve every branch in a completed method; foreign destinations are an ownership error.
    pub fn link(&mut self) {
        assert!(
            self.fixups
                .iter()
                .all(|(_, label)| label.builder == self.id)
                && self
                    .switch_fixups
                    .iter()
                    .all(|(_, _, label)| label.builder == self.id),
            "an external inline branch reached final method linking"
        );
        self.link_local_branches();
    }
}
