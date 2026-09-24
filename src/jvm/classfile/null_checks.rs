//! kotlinc's redundant null-check elimination (`RedundantNullCheckMethodTransformer`), first part:
//! an `Intrinsics.checkNotNull*` call on a value already known not to be `null` is removed.
//!
//! kotlinc runs a nullability analysis over the method. A value is known non-null when it was just
//! created (`new`, a string or class constant, a new array, `Unit.INSTANCE`, a boxing `valueOf`),
//! and a local becomes known non-null — or `null` — past a check of it: on the non-null edge of an
//! `ifnull`/`ifnonnull` of the local, and after a `checkNotNull*` or `checkNotNullParameter` of it.
//! Where two paths meet, a local keeps a fact only when both paths agree. Then:
//!
//! - `dup|aload; invokestatic checkNotNull(Object)` goes;
//! - `dup|aload; ldc "…"; invokestatic checkNotNull(Object, String)` goes;
//! - `dup|aload; ldc "…"; invokestatic checkNotNullExpressionValue` goes.
//!
//! This models facts on locals and resolves the checked value's producer only through straight-line
//! code, a subset of kotlinc's analysis: whatever it proves, kotlinc proves too. A check it cannot
//! prove redundant stays, as it was.

use super::bytecode_analysis::ControlGraph;
use super::{ClassWriter, VerifType};
use crate::jvm::inline::{BranchTarget, Insn};
use std::collections::VecDeque;

/// What an `invokestatic` or `getstatic` operand is, as far as this pass cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    /// `Intrinsics.checkNotNull(Object)`.
    CheckNotNull,
    /// `Intrinsics.checkNotNull(Object, String)`.
    CheckNotNullWithMessage,
    /// `Intrinsics.checkNotNullExpressionValue` / `checkExpressionValueIsNotNull`.
    CheckExpressionValue,
    /// `Intrinsics.checkNotNullParameter` / `checkParameterIsNotNull`.
    CheckParameter,
    /// An `Intrinsics.throw…` helper: control does not return.
    Throws,
    /// A primitive boxing call (`Integer.valueOf(int)`, …): its result is never `null`.
    Boxing,
}

/// Constant-pool facts at the finished-class boundary.
trait PoolView {
    fn call(&self, method: u16) -> Option<Call>;
    fn unit_instance(&self, field: u16) -> bool;
    fn reference_constant(&self, index: u16) -> bool;
}

impl PoolView for ClassWriter {
    fn call(&self, method: u16) -> Option<Call> {
        let (owner, name, descriptor) = self.methodref_parts(method)?;
        match (owner, name, descriptor) {
            ("kotlin/jvm/internal/Intrinsics", "checkNotNull", "(Ljava/lang/Object;)V") => {
                Some(Call::CheckNotNull)
            }
            (
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNull",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            ) => Some(Call::CheckNotNullWithMessage),
            (
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullExpressionValue" | "checkExpressionValueIsNotNull",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            ) => Some(Call::CheckExpressionValue),
            (
                "kotlin/jvm/internal/Intrinsics",
                "checkNotNullParameter" | "checkParameterIsNotNull",
                "(Ljava/lang/Object;Ljava/lang/String;)V",
            ) => Some(Call::CheckParameter),
            (
                "kotlin/jvm/internal/Intrinsics",
                "throwNpe"
                | "throwAssert"
                | "throwIllegalArgument"
                | "throwIllegalState"
                | "throwUndefinedForReified",
                "()V" | "(Ljava/lang/String;)V",
            )
            | (
                "kotlin/jvm/internal/Intrinsics",
                "throwUninitializedProperty"
                | "throwUninitializedPropertyAccessException"
                | "throwParameterIsNullException",
                "(Ljava/lang/String;)V",
            ) => Some(Call::Throws),
            ("java/lang/Boolean", "valueOf", "(Z)Ljava/lang/Boolean;")
            | ("java/lang/Character", "valueOf", "(C)Ljava/lang/Character;")
            | ("java/lang/Byte", "valueOf", "(B)Ljava/lang/Byte;")
            | ("java/lang/Short", "valueOf", "(S)Ljava/lang/Short;")
            | ("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;")
            | ("java/lang/Long", "valueOf", "(J)Ljava/lang/Long;")
            | ("java/lang/Float", "valueOf", "(F)Ljava/lang/Float;")
            | ("java/lang/Double", "valueOf", "(D)Ljava/lang/Double;") => Some(Call::Boxing),
            _ => None,
        }
    }

    fn unit_instance(&self, field: u16) -> bool {
        self.cp.fieldref_parts(field) == Some(("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;"))
    }

    fn reference_constant(&self, index: u16) -> bool {
        matches!(
            self.loadable_constant_type_at(index),
            Some(VerifType::Object(_) | VerifType::ObjectName(_))
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nullness {
    Null,
    NotNull,
    Unknown,
}

impl Nullness {
    fn join(self, other: Nullness) -> Nullness {
        if self == other {
            self
        } else {
            Nullness::Unknown
        }
    }
}

const ACONST_NULL: u8 = 0x01;
const LDC: u8 = 0x12;
const LDC_W: u8 = 0x13;
const DUP: u8 = 0x59;
const IFNULL: u8 = 0xc6;
const IFNONNULL: u8 = 0xc7;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;
const NEW: u8 = 0xbb;
const NEWARRAY: u8 = 0xbc;
const ANEWARRAY: u8 = 0xbd;
const CHECKCAST: u8 = 0xc0;

fn u2(operands: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*operands.first()?, *operands.get(1)?]))
}

/// The slot an `aload` reads.
fn aload_slot(insn: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    match *op {
        0x19 => operands.first().map(|&slot| u16::from(slot)),
        0x2a..=0x2d => Some(u16::from(*op - 0x2a)),
        0xc4 if operands.first() == Some(&0x19) => {
            Some(u16::from_be_bytes([*operands.get(1)?, *operands.get(2)?]))
        }
        _ => None,
    }
}

/// The slot and width a store writes.
fn store_slot(insn: &Insn) -> Option<(u16, bool, bool)> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    let (slot, kind) = match *op {
        0x36..=0x3a => (u16::from(*operands.first()?), *op - 0x36),
        0x3b..=0x4e => (u16::from((*op - 0x3b) % 4), (*op - 0x3b) / 4),
        0xc4 => {
            let inner = *operands.first()?;
            if !(0x36..=0x3a).contains(&inner) {
                return None;
            }
            (
                u16::from_be_bytes([*operands.get(1)?, *operands.get(2)?]),
                inner - 0x36,
            )
        }
        _ => return None,
    };
    // (slot, wide, reference)
    Some((slot, matches!(kind, 1 | 3), kind == 4))
}

fn opcode(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Plain { op, .. } | Insn::Branch { op, .. } | Insn::BranchW { op, .. } => Some(*op),
        _ => None,
    }
}

struct Analysis<'a> {
    insns: &'a [Insn],
    /// `true` where a branch, switch or handler arrives at an index.
    arrivals: &'a [bool],
    pool: &'a dyn PoolView,
    /// The first instruction in each straight-line `dup`/`checkcast` producer chain.
    producer_roots: Vec<usize>,
}

impl Analysis<'_> {
    fn new<'a>(insns: &'a [Insn], arrivals: &'a [bool], pool: &'a dyn PoolView) -> Analysis<'a> {
        let mut producer_roots: Vec<usize> = (0..insns.len()).collect();
        for at in 1..insns.len() {
            if matches!(opcode(&insns[at]), Some(DUP | CHECKCAST)) && !arrivals[at] {
                producer_roots[at] = producer_roots[at - 1];
            }
        }
        Analysis {
            insns,
            arrivals,
            pool,
            producer_roots,
        }
    }

    fn call(&self, at: usize) -> Option<Call> {
        match &self.insns[at] {
            Insn::Plain {
                op: INVOKESTATIC,
                operands,
            } => self.pool.call(u2(operands)?),
            _ => None,
        }
    }

    /// The nullness of the value instruction `at` leaves on top of the stack, from `locals` as they
    /// stand before it, following only straight-line producers.
    fn produced(&self, at: usize, locals: &[Nullness]) -> Nullness {
        let at = self.producer_roots.get(at).copied().unwrap_or(at);
        let insn = &self.insns[at];
        if let Some(slot) = aload_slot(insn) {
            return locals
                .get(usize::from(slot))
                .copied()
                .unwrap_or(Nullness::Unknown);
        }
        let Insn::Plain { op, operands } = insn else {
            return Nullness::Unknown;
        };
        match *op {
            ACONST_NULL => Nullness::Null,
            NEW | NEWARRAY | ANEWARRAY => Nullness::NotNull,
            LDC => {
                if operands
                    .first()
                    .is_some_and(|&index| self.pool.reference_constant(u16::from(index)))
                {
                    Nullness::NotNull
                } else {
                    Nullness::Unknown
                }
            }
            LDC_W => {
                if u2(operands).is_some_and(|index| self.pool.reference_constant(index)) {
                    Nullness::NotNull
                } else {
                    Nullness::Unknown
                }
            }
            GETSTATIC if u2(operands).is_some_and(|field| self.pool.unit_instance(field)) => {
                Nullness::NotNull
            }
            INVOKESTATIC if self.call(at) == Some(Call::Boxing) => Nullness::NotNull,
            _ => Nullness::Unknown,
        }
    }

    /// The local an instruction checks: the `aload` right before it, or before the `dup` right
    /// before it, with nothing arriving in between.
    fn checked_local(&self, at: usize, skip: usize) -> Option<u16> {
        let mut load = at.checked_sub(1 + skip)?;
        if (load + 1..=at).any(|index| self.arrivals[index]) {
            return None;
        }
        if opcode(&self.insns[load]) == Some(DUP) {
            load = load.checked_sub(1)?;
            if self.arrivals[load + 1] {
                return None;
            }
        }
        aload_slot(&self.insns[load])
    }
}

/// Nullness states retained across the method. This is a hard ceiling, not a heuristic: without it
/// a legal 65,535-byte method with 65,535 local slots could allocate billions of analysis cells.
const ANALYSIS_CELL_LIMIT: usize = 50 * 1024 * 1024;

fn analysis_within_limit(instructions: usize, locals: usize) -> bool {
    instructions
        .checked_add(1)
        .and_then(|points| points.checked_mul(locals.max(1)))
        .is_some_and(|cells| cells <= ANALYSIS_CELL_LIMIT)
}

/// One candidate's value producer and all original instructions removed with the check.
struct Candidate {
    value_at: usize,
    group: Vec<usize>,
}

fn candidates(analysis: &Analysis<'_>) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for at in 0..analysis.insns.len() {
        let Some(call) = analysis.call(at) else {
            continue;
        };
        let (value_at, group) = match call {
            Call::CheckNotNull => {
                let Some(feed) = at.checked_sub(1) else {
                    continue;
                };
                (feed, vec![feed, at])
            }
            Call::CheckNotNullWithMessage | Call::CheckExpressionValue => {
                let Some(feed) = at.checked_sub(2) else {
                    continue;
                };
                if !matches!(opcode(&analysis.insns[feed + 1]), Some(LDC | LDC_W)) {
                    continue;
                }
                (feed, vec![feed, feed + 1, at])
            }
            _ => continue,
        };
        let feed = &analysis.insns[value_at];
        if (opcode(feed) == Some(DUP) || aload_slot(feed).is_some())
            && !(value_at + 1..=at).any(|index| analysis.arrivals[index])
        {
            candidates.push(Candidate { value_at, group });
        }
    }
    candidates
}

/// One bit per original instruction: every redundant `checkNotNull*` call and the load or `dup`
/// (plus message constant) that feeds it.
fn redundant(
    insns: &[Insn],
    graph: &ControlGraph,
    arrivals: &[bool],
    max_locals: usize,
    pool: &dyn PoolView,
) -> Vec<bool> {
    let analysis = Analysis::new(insns, arrivals, pool);
    let candidates = candidates(&analysis);
    let mut removed = vec![false; insns.len()];
    if candidates.is_empty() || !analysis_within_limit(insns.len(), max_locals) {
        return removed;
    }
    let mut before: Vec<Option<Vec<Nullness>>> = vec![None; insns.len() + 1];
    // Parameters and `this` are unknown until checked.
    before[0] = Some(vec![Nullness::Unknown; max_locals]);
    let mut pending = VecDeque::from([0usize]);
    let mut queued = vec![false; insns.len() + 1];
    queued[0] = true;
    while let Some(at) = pending.pop_front() {
        queued[at] = false;
        if at >= insns.len() {
            continue;
        }
        let Some(state) = before[at].clone() else {
            continue;
        };
        let mut after = state.clone();
        if let Some((slot, wide, reference)) = store_slot(&insns[at]) {
            let slot = usize::from(slot);
            if slot < after.len() {
                after[slot] = if reference && at > 0 && !arrivals[at] {
                    analysis.produced(at - 1, &state)
                } else {
                    Nullness::Unknown
                };
            }
            if wide && slot + 1 < after.len() {
                after[slot + 1] = Nullness::Unknown;
            }
        }
        let call = analysis.call(at);
        let asserted = match call {
            Some(Call::CheckNotNull) => analysis.checked_local(at, 0),
            Some(
                Call::CheckNotNullWithMessage | Call::CheckExpressionValue | Call::CheckParameter,
            ) => analysis.checked_local(at, 1),
            _ => None,
        };
        if let Some(slot) = asserted {
            if let Some(local) = after.get_mut(usize::from(slot)) {
                *local = Nullness::NotNull;
            }
        }
        let null_check = match &insns[at] {
            Insn::Branch {
                op: op @ (IFNULL | IFNONNULL),
                target: BranchTarget::Internal(to),
            } if *to != at + 1 => analysis
                .checked_local(at, 0)
                .map(|slot| (usize::from(slot), *op == IFNULL, *to)),
            _ => None,
        };
        let mut propagate = |to: usize, incoming: &[Nullness]| {
            if to > insns.len() {
                return;
            }
            let changed = match &mut before[to] {
                Some(current) => {
                    let mut changed = false;
                    for (mine, theirs) in current.iter_mut().zip(incoming) {
                        let met = mine.join(*theirs);
                        changed |= met != *mine;
                        *mine = met;
                    }
                    changed
                }
                slot @ None => {
                    *slot = Some(incoming.to_vec());
                    true
                }
            };
            if changed && !queued[to] {
                queued[to] = true;
                pending.push_back(to);
            }
        };
        if call != Some(Call::Throws) {
            for &to in graph.normal_successors(at) {
                let mut incoming = after.clone();
                if let Some((slot, is_ifnull, target)) = null_check {
                    if slot < incoming.len() {
                        let jumped = to == target;
                        // `ifnull` jumps on null; `ifnonnull` on non-null.
                        incoming[slot] = if jumped == is_ifnull {
                            Nullness::Null
                        } else {
                            Nullness::NotNull
                        };
                    }
                }
                propagate(to, &incoming);
            }
        }
        for &handler in graph.exceptional_successors(at) {
            // The throw happens before a call returns or a store completes. For other instructions
            // keep the deliberately conservative pre/post join used by the shared graph.
            propagate(handler, &state);
            if call != Some(Call::Throws) {
                propagate(handler, &after);
            }
        }
    }

    for candidate in candidates {
        let Some(state) = before[candidate.value_at].as_ref() else {
            continue;
        };
        if analysis.produced(candidate.value_at, state) == Nullness::NotNull {
            for index in candidate.group {
                removed[index] = true;
            }
        }
    }
    removed
}

impl ClassWriter {
    pub(super) fn redundant_null_checks(
        &self,
        insns: &[Insn],
        graph: &ControlGraph,
        arrivals: &[bool],
        max_locals: usize,
    ) -> Vec<bool> {
        redundant(insns, graph, arrivals, max_locals, self)
    }
}

#[cfg(test)]
mod tests {
    use super::super::bytecode_analysis::Handler;
    use super::*;

    fn op(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn with(op: u8, operands: &[u8]) -> Insn {
        Insn::Plain {
            op,
            operands: operands.to_vec(),
        }
    }

    fn branch(op: u8, to: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        }
    }

    // Pool: methodref 1 = checkNotNull(Object, String), 2 = checkNotNullParameter,
    // 3 = checkNotNull(Object), 4 = checkNotNullExpressionValue, 5 = an Intrinsics throw helper;
    // ldc 7 is a string.
    struct TestPool;

    impl PoolView for TestPool {
        fn call(&self, method: u16) -> Option<Call> {
            match method {
                1 => Some(Call::CheckNotNullWithMessage),
                2 => Some(Call::CheckParameter),
                3 => Some(Call::CheckNotNull),
                4 => Some(Call::CheckExpressionValue),
                5 => Some(Call::Throws),
                _ => None,
            }
        }

        fn unit_instance(&self, _field: u16) -> bool {
            false
        }

        fn reference_constant(&self, index: u16) -> bool {
            index == 7
        }
    }

    fn run_with_handlers_and_locals(
        insns: &[Insn],
        handlers: &[Handler],
        max_locals: usize,
    ) -> Vec<usize> {
        let graph = ControlGraph::build(insns, handlers).expect("graph");
        let mut arrivals = vec![false; insns.len() + 1];
        for insn in insns {
            if let Insn::Branch {
                target: BranchTarget::Internal(to),
                ..
            } = insn
            {
                arrivals[*to] = true;
            }
        }
        for handler in handlers {
            arrivals[handler.start] = true;
            arrivals[handler.end] = true;
            arrivals[handler.handler] = true;
        }
        redundant(insns, &graph, &arrivals, max_locals, &TestPool)
            .into_iter()
            .enumerate()
            .filter_map(|(index, removed)| removed.then_some(index))
            .collect()
    }

    fn run_with_handlers(insns: &[Insn], handlers: &[Handler]) -> Vec<usize> {
        run_with_handlers_and_locals(insns, handlers, 4)
    }

    fn run(insns: &[Insn]) -> Vec<usize> {
        run_with_handlers(insns, &[])
    }

    const ALOAD_0: u8 = 0x2a;
    const ARETURN: u8 = 0xb0;

    #[test]
    fn runtime_facts_require_complete_owner_name_and_descriptor_identity() {
        let mut writer = ClassWriter::new("fixture/Checks", "java/lang/Object");
        let boxing = writer.methodref("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;");
        let wrong_boxing_result =
            writer.methodref("java/lang/Integer", "valueOf", "(I)Ljava/lang/Object;");
        let unit = writer.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
        let wrong_unit_type = writer.fieldref("kotlin/Unit", "INSTANCE", "Ljava/lang/Object;");
        let throwing = writer.methodref("kotlin/jvm/internal/Intrinsics", "throwNpe", "()V");
        let wrong_throwing_result = writer.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "throwNpe",
            "()Ljava/lang/Object;",
        );
        assert_eq!(PoolView::call(&writer, boxing), Some(Call::Boxing));
        assert_eq!(PoolView::call(&writer, wrong_boxing_result), None);
        assert_eq!(PoolView::call(&writer, throwing), Some(Call::Throws));
        assert_eq!(PoolView::call(&writer, wrong_throwing_result), None);
        assert!(PoolView::unit_instance(&writer, unit));
        assert!(!PoolView::unit_instance(&writer, wrong_unit_type));
    }

    #[test]
    fn a_cast_of_a_checked_parameter_needs_no_null_check() {
        // `fun q(a: Any) = a as String`: 0 aload_0; 1 ldc; 2 checkNotNullParameter; 3 aload_0;
        // 4 dup; 5 ldc; 6 checkNotNull(Object, String); 7 checkcast; 8 areturn.
        let insns = [
            op(ALOAD_0),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 2]),
            op(ALOAD_0),
            op(DUP),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 1]),
            with(CHECKCAST, &[0, 9]),
            op(ARETURN),
        ];
        assert_eq!(run(&insns), vec![4, 5, 6]);
    }

    #[test]
    fn an_unchecked_parameter_keeps_its_null_check() {
        let insns = [
            op(ALOAD_0),
            op(DUP),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 1]),
            with(CHECKCAST, &[0, 9]),
            op(ARETURN),
        ];
        assert_eq!(run(&insns), Vec::<usize>::new());
    }

    #[test]
    fn a_non_null_fact_crosses_an_arbitrarily_deep_local_chain() {
        let mut insns = vec![op(ALOAD_0), with(LDC, &[7]), with(INVOKESTATIC, &[0, 2])];
        for slot in 1..=10u8 {
            insns.push(with(0x19, &[slot - 1])); // aload
            insns.push(with(0x3a, &[slot])); // astore
        }
        let removed_from = insns.len() + 1;
        insns.extend([
            with(0x19, &[10]),
            op(DUP),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 1]),
            op(ARETURN),
        ]);
        assert_eq!(
            run_with_handlers_and_locals(&insns, &[], 11),
            vec![removed_from, removed_from + 1, removed_from + 2]
        );
    }

    #[test]
    fn matching_non_null_branch_stores_keep_the_fact_at_the_merge() {
        let insns = [
            with(LDC, &[7]),
            op(0x4c), // astore_1
            op(ALOAD_0),
            branch(IFNULL, 7),
            with(LDC, &[7]),
            op(0x4c),        // astore_1
            branch(0xa7, 9), // goto merge
            with(LDC, &[7]),
            op(0x4c), // astore_1
            op(0x2b), // aload_1
            op(DUP),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 1]),
            op(ARETURN),
        ];
        assert_eq!(run(&insns), vec![10, 11, 12]);
    }

    #[test]
    fn a_local_is_non_null_past_its_ifnull_only() {
        // 0 aload_0; 1 ifnull 6; 2 aload_0; 3 dup; 4 invokestatic checkNotNull(Object);
        // 5 areturn; 6 aload_0; 7 dup; 8 invokestatic checkNotNull(Object); 9 areturn
        let insns = [
            op(ALOAD_0),
            branch(IFNULL, 6),
            op(ALOAD_0),
            op(DUP),
            with(INVOKESTATIC, &[0, 3]),
            op(ARETURN),
            op(ALOAD_0),
            op(DUP),
            with(INVOKESTATIC, &[0, 3]),
            op(ARETURN),
        ];
        assert_eq!(run(&insns), vec![3, 4]);
    }

    #[test]
    fn a_new_string_constant_needs_no_expression_check() {
        // 0 ldc "s"; 1 dup; 2 ldc; 3 checkNotNullExpressionValue; 4 areturn
        let insns = [
            with(LDC, &[7]),
            op(DUP),
            with(LDC, &[7]),
            with(INVOKESTATIC, &[0, 4]),
            op(ARETURN),
        ];
        assert_eq!(run(&insns), vec![1, 2, 3]);
    }

    #[test]
    fn a_throw_helper_still_reaches_its_exception_handler() {
        // Two disjoint protected ranges share one handler. The first reaches it with local 1
        // non-null; the Intrinsics throw reaches it with local 1 null. Omitting the throw's
        // exceptional edge would incorrectly remove the handler's check.
        let insns = [
            op(0x03),                    // 0 iconst_0
            branch(0x99, 6),             // 1 ifeq null_path
            with(LDC, &[7]),             // 2 non-null String
            op(0x4c),                    // 3 astore_1
            branch(0xa7, 9),             // 4 goto first protected call
            op(0x00),                    // 5 nop
            op(ACONST_NULL),             // 6 null
            op(0x4c),                    // 7 astore_1
            branch(0xa7, 11),            // 8 goto throwing helper
            with(INVOKESTATIC, &[0, 6]), // 9 an ordinary possibly-throwing call
            branch(0xa7, 17),            // 10 leave
            with(INVOKESTATIC, &[0, 5]), // 11 Intrinsics throw helper
            branch(0xa7, 17),            // 12 unreachable normal edge
            op(0x2b),                    // 13 handler: aload_1
            op(DUP),                     // 14
            with(INVOKESTATIC, &[0, 3]), // 15 checkNotNull(Object)
            op(ARETURN),                 // 16
            op(0xb1),                    // 17 return
        ];
        let handlers = [
            Handler {
                start: 9,
                end: 10,
                handler: 13,
            },
            Handler {
                start: 11,
                end: 12,
                handler: 13,
            },
        ];
        assert_eq!(run_with_handlers(&insns, &handlers), Vec::<usize>::new());
    }

    #[test]
    fn nullness_analysis_has_a_checked_size_ceiling() {
        assert!(analysis_within_limit(1_000, 50));
        assert!(!analysis_within_limit(usize::MAX, 2));
        assert!(!analysis_within_limit(65_535, 65_535));
    }
}
