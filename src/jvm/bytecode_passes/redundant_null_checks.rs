//! kotlinc's `RedundantNullCheckMethodTransformer`, first part, over a [`MethodNode`]: an
//! `Intrinsics.checkNotNull*` call on a value already known not to be `null` is removed.
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
//! prove redundant stays, as it was. A label a jump, switch or protected range names breaks a
//! straight line; one only a line number or a local's range names does not.

use std::collections::{BTreeSet, VecDeque};

use super::instruction_graph::InstructionGraph;
use crate::jvm::method_node::{Constant, Insn, LabelId, MethodNode, Node};

const INTRINSICS: &str = "kotlin/jvm/internal/Intrinsics";
const ACONST_NULL: u8 = 0x01;
const ALOAD: u8 = 0x19;
const ISTORE: u8 = 0x36;
const LSTORE: u8 = 0x37;
const DSTORE: u8 = 0x39;
const ASTORE: u8 = 0x3a;
const DUP: u8 = 0x59;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;
const NEW: u8 = 0xbb;
const NEWARRAY: u8 = 0xbc;
const ANEWARRAY: u8 = 0xbd;
const CHECKCAST: u8 = 0xc0;
const IFNULL: u8 = 0xc6;
const IFNONNULL: u8 = 0xc7;

/// What an `invokestatic` is, as far as this pass cares.
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

fn call(insn: &Insn) -> Option<Call> {
    let Insn::Method {
        op: INVOKESTATIC,
        owner,
        name,
        desc,
        ..
    } = insn
    else {
        return None;
    };
    match (owner.as_str(), name.as_str(), desc.as_str()) {
        (INTRINSICS, "checkNotNull", "(Ljava/lang/Object;)V") => Some(Call::CheckNotNull),
        (INTRINSICS, "checkNotNull", "(Ljava/lang/Object;Ljava/lang/String;)V") => {
            Some(Call::CheckNotNullWithMessage)
        }
        (
            INTRINSICS,
            "checkNotNullExpressionValue" | "checkExpressionValueIsNotNull",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        ) => Some(Call::CheckExpressionValue),
        (
            INTRINSICS,
            "checkNotNullParameter" | "checkParameterIsNotNull",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        ) => Some(Call::CheckParameter),
        (
            INTRINSICS,
            "throwNpe"
            | "throwAssert"
            | "throwIllegalArgument"
            | "throwIllegalState"
            | "throwUndefinedForReified",
            "()V" | "(Ljava/lang/String;)V",
        )
        | (
            INTRINSICS,
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

/// The slot an `aload` reads.
fn aload_slot(insn: &Insn) -> Option<u16> {
    match *insn {
        Insn::Var { op: ALOAD, slot } => Some(slot),
        _ => None,
    }
}

/// The slot a store writes, whether it is two words wide, and whether it stores a reference.
fn store_slot(insn: &Insn) -> Option<(u16, bool, bool)> {
    match *insn {
        Insn::Var { op, slot } if (ISTORE..=ASTORE).contains(&op) => {
            Some((slot, matches!(op, LSTORE | DSTORE), op == ASTORE))
        }
        _ => None,
    }
}

fn opcode(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Op(op) | Insn::Type { op, .. } | Insn::Jump { op, .. } => Some(*op),
        _ => None,
    }
}

/// A constant `ldc` pushes as a reference: a string, a class, a method type or a method handle.
fn is_reference_constant(constant: &Constant) -> bool {
    matches!(
        constant,
        Constant::String(_) | Constant::Class(_) | Constant::MethodType(_) | Constant::Handle(_)
    )
}

struct Analysis<'a> {
    graph: &'a InstructionGraph<'a>,
    /// `true` where a jump, switch or protected range names a label in front of an instruction.
    arrivals: Vec<bool>,
    /// The first instruction in each straight-line `dup`/`checkcast` producer chain.
    producer_roots: Vec<usize>,
}

impl Analysis<'_> {
    fn new<'a>(graph: &'a InstructionGraph<'a>, method: &MethodNode) -> Analysis<'a> {
        let mut arriving: BTreeSet<LabelId> = BTreeSet::new();
        for node in &method.nodes {
            if let Node::Insn(insn) = node {
                arriving.extend(insn.jump_targets());
            }
        }
        for block in &method.try_catch_blocks {
            arriving.extend([block.start, block.end, block.handler]);
        }
        let n = graph.len();
        let arrivals: Vec<bool> = (0..=n)
            .map(|at| {
                graph
                    .labels_before(at)
                    .any(|label| arriving.contains(&label))
            })
            .collect();
        let mut producer_roots: Vec<usize> = (0..n).collect();
        for at in 1..n {
            if matches!(opcode(graph.insn(at)), Some(DUP | CHECKCAST)) && !arrivals[at] {
                producer_roots[at] = producer_roots[at - 1];
            }
        }
        Analysis {
            graph,
            arrivals,
            producer_roots,
        }
    }

    fn call(&self, at: usize) -> Option<Call> {
        call(self.graph.insn(at))
    }

    /// The nullness of the value instruction `at` leaves on top of the stack, from `locals` as they
    /// stand before it, following only straight-line producers.
    fn produced(&self, at: usize, locals: &[Nullness]) -> Nullness {
        let at = self.producer_roots.get(at).copied().unwrap_or(at);
        let insn = self.graph.insn(at);
        if let Some(slot) = aload_slot(insn) {
            return locals
                .get(usize::from(slot))
                .copied()
                .unwrap_or(Nullness::Unknown);
        }
        match insn {
            Insn::Op(ACONST_NULL) => Nullness::Null,
            Insn::Type {
                op: NEW | ANEWARRAY,
                ..
            }
            | Insn::Int { op: NEWARRAY, .. } => Nullness::NotNull,
            Insn::Ldc(constant) if is_reference_constant(constant) => Nullness::NotNull,
            Insn::Field {
                op: GETSTATIC,
                owner,
                name,
                desc,
            } if owner == "kotlin/Unit" && name == "INSTANCE" && desc == "Lkotlin/Unit;" => {
                Nullness::NotNull
            }
            _ if self.call(at) == Some(Call::Boxing) => Nullness::NotNull,
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
        if opcode(self.graph.insn(load)) == Some(DUP) {
            load = load.checked_sub(1)?;
            if self.arrivals[load + 1] {
                return None;
            }
        }
        aload_slot(self.graph.insn(load))
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

/// One candidate's value producer and every instruction removed with the check.
struct Candidate {
    value_at: usize,
    group: Vec<usize>,
}

fn candidates(analysis: &Analysis<'_>) -> Vec<Candidate> {
    let graph = analysis.graph;
    let mut candidates = Vec::new();
    for at in 0..graph.len() {
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
                if !matches!(graph.insn(feed + 1), Insn::Ldc(constant) if !constant.is_wide()) {
                    continue;
                }
                (feed, vec![feed, feed + 1, at])
            }
            _ => continue,
        };
        let feed = graph.insn(value_at);
        if (opcode(feed) == Some(DUP) || aload_slot(feed).is_some())
            && !(value_at + 1..=at).any(|index| analysis.arrivals[index])
        {
            candidates.push(Candidate { value_at, group });
        }
    }
    candidates
}

/// The instructions (by number in `graph`) of every redundant `checkNotNull*` call and the load or
/// `dup` (plus message constant) that feeds it.
fn redundant(method: &MethodNode, graph: &InstructionGraph<'_>) -> BTreeSet<usize> {
    let analysis = Analysis::new(graph, method);
    let candidates = candidates(&analysis);
    let n = graph.len();
    let max_locals = usize::from(method.max_locals);
    let mut removed = BTreeSet::new();
    if candidates.is_empty() || !analysis_within_limit(n, max_locals) {
        return removed;
    }
    let mut before: Vec<Option<Vec<Nullness>>> = vec![None; n + 1];
    // Parameters and `this` are unknown until checked.
    before[0] = Some(vec![Nullness::Unknown; max_locals]);
    let mut pending = VecDeque::from([0usize]);
    let mut queued = vec![false; n + 1];
    queued[0] = true;
    while let Some(at) = pending.pop_front() {
        queued[at] = false;
        if at >= n {
            continue;
        }
        let Some(state) = before[at].clone() else {
            continue;
        };
        let insn = graph.insn(at);
        let mut after = state.clone();
        if let Some((slot, wide, reference)) = store_slot(insn) {
            let slot = usize::from(slot);
            if slot < after.len() {
                after[slot] = if reference && at > 0 && !analysis.arrivals[at] {
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
        let null_check = match insn {
            Insn::Jump {
                op: op @ (IFNULL | IFNONNULL),
                target,
            } if graph.at_label(*target) != at + 1 => analysis
                .checked_local(at, 0)
                .map(|slot| (usize::from(slot), *op == IFNULL, graph.at_label(*target))),
            _ => None,
        };
        let mut propagate = |to: usize, incoming: &[Nullness]| {
            if to > n {
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
            removed.extend(candidate.group);
        }
    }
    removed
}

/// The node positions of every redundant null check in `method` and the instructions feeding it,
/// in order; empty when the method's control flow is outside what the analysis models.
pub(crate) fn select(method: &MethodNode) -> Vec<usize> {
    let Some(graph) = InstructionGraph::build(method) else {
        return Vec::new();
    };
    redundant(method, &graph)
        .into_iter()
        .map(|index| graph.position(index))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::TryCatchBlock;

    const ALOAD_0: Insn = Insn::Var { op: ALOAD, slot: 0 };
    const ARETURN: u8 = 0xb0;
    const GOTO: u8 = 0xa7;

    enum I {
        Insn(Insn),
        Jump(u8, usize),
    }

    fn op(op: u8) -> I {
        I::Insn(Insn::Op(op))
    }

    fn insn(insn: Insn) -> I {
        I::Insn(insn)
    }

    fn jump(op: u8, to: usize) -> I {
        I::Jump(op, to)
    }

    fn aload(slot: u16) -> I {
        I::Insn(Insn::Var { op: ALOAD, slot })
    }

    fn astore(slot: u16) -> I {
        I::Insn(Insn::Var { op: ASTORE, slot })
    }

    fn string() -> I {
        I::Insn(Insn::Ldc(Constant::String("s".into())))
    }

    fn intrinsic(name: &str, desc: &str) -> I {
        I::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner: INTRINSICS.to_string(),
            name: name.to_string(),
            desc: desc.to_string(),
            interface: false,
        })
    }

    fn check_with_message() -> I {
        intrinsic("checkNotNull", "(Ljava/lang/Object;Ljava/lang/String;)V")
    }

    fn check_parameter() -> I {
        intrinsic(
            "checkNotNullParameter",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        )
    }

    fn check() -> I {
        intrinsic("checkNotNull", "(Ljava/lang/Object;)V")
    }

    fn check_expression() -> I {
        intrinsic(
            "checkNotNullExpressionValue",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        )
    }

    fn throw_npe() -> I {
        intrinsic("throwNpe", "()V")
    }

    fn cast() -> I {
        insn(Insn::Type {
            op: CHECKCAST,
            class: "java/lang/String".to_string(),
        })
    }

    /// `insns` with a label in front of each, protected by `handlers` as `(start, end, handler)`
    /// instruction numbers; the removed instructions by number.
    fn run_with_handlers_and_locals(
        insns: Vec<I>,
        handlers: &[(usize, usize, usize)],
        max_locals: u16,
    ) -> Vec<usize> {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        method.max_locals = max_locals;
        let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
        for (k, insn) in insns.into_iter().enumerate() {
            method.nodes.push(Node::Label(labels[k]));
            method.nodes.push(Node::Insn(match insn {
                I::Insn(insn) => insn,
                I::Jump(op, to) => Insn::Jump {
                    op,
                    target: labels[to],
                },
            }));
        }
        method
            .nodes
            .push(Node::Label(*labels.last().expect("an end label")));
        for &(start, end, handler) in handlers {
            method.try_catch_blocks.push(TryCatchBlock {
                start: labels[start],
                end: labels[end],
                handler: labels[handler],
                catch_type: None,
            });
        }
        // Every instruction stands at node 2k + 1, behind its label.
        select(&method)
            .into_iter()
            .map(|position| (position - 1) / 2)
            .collect()
    }

    fn run_with_handlers(insns: Vec<I>, handlers: &[(usize, usize, usize)]) -> Vec<usize> {
        run_with_handlers_and_locals(insns, handlers, 4)
    }

    fn run(insns: Vec<I>) -> Vec<usize> {
        run_with_handlers(insns, &[])
    }

    #[test]
    fn runtime_facts_require_complete_owner_name_and_descriptor_identity() {
        let method = |owner: &str, name: &str, desc: &str| Insn::Method {
            op: INVOKESTATIC,
            owner: owner.to_string(),
            name: name.to_string(),
            desc: desc.to_string(),
            interface: false,
        };
        assert_eq!(
            call(&method(
                "java/lang/Integer",
                "valueOf",
                "(I)Ljava/lang/Integer;"
            )),
            Some(Call::Boxing)
        );
        assert_eq!(
            call(&method(
                "java/lang/Integer",
                "valueOf",
                "(I)Ljava/lang/Object;"
            )),
            None
        );
        assert_eq!(
            call(&method(INTRINSICS, "throwNpe", "()V")),
            Some(Call::Throws)
        );
        assert_eq!(
            call(&method(INTRINSICS, "throwNpe", "()Ljava/lang/Object;")),
            None
        );
        assert_eq!(
            call(&method(
                "fixture/Intrinsics",
                "checkNotNull",
                "(Ljava/lang/Object;)V"
            )),
            None
        );
        let unit = |desc: &str| Insn::Field {
            op: GETSTATIC,
            owner: "kotlin/Unit".to_string(),
            name: "INSTANCE".to_string(),
            desc: desc.to_string(),
        };
        let mut method = MethodNode::new(0x0009, "f", "()V");
        method.max_locals = 1;
        method.nodes = vec![
            Node::Insn(unit("Lkotlin/Unit;")),
            Node::Insn(Insn::Op(DUP)),
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: INTRINSICS.to_string(),
                name: "checkNotNull".to_string(),
                desc: "(Ljava/lang/Object;)V".to_string(),
                interface: false,
            }),
            Node::Insn(Insn::Op(ARETURN)),
        ];
        assert_eq!(select(&method), vec![1, 2]);
        method.nodes[0] = Node::Insn(unit("Ljava/lang/Object;"));
        assert_eq!(select(&method), Vec::<usize>::new());
    }

    #[test]
    fn a_cast_of_a_checked_parameter_needs_no_null_check() {
        // `fun q(a: Any) = a as String`: 0 aload_0; 1 ldc; 2 checkNotNullParameter; 3 aload_0;
        // 4 dup; 5 ldc; 6 checkNotNull(Object, String); 7 checkcast; 8 areturn.
        let insns = vec![
            aload(0),
            string(),
            check_parameter(),
            aload(0),
            op(DUP),
            string(),
            check_with_message(),
            cast(),
            op(ARETURN),
        ];
        assert_eq!(run(insns), vec![4, 5, 6]);
    }

    #[test]
    fn an_unchecked_parameter_keeps_its_null_check() {
        let insns = vec![
            insn(ALOAD_0),
            op(DUP),
            string(),
            check_with_message(),
            cast(),
            op(ARETURN),
        ];
        assert_eq!(run(insns), Vec::<usize>::new());
    }

    #[test]
    fn a_non_null_fact_crosses_an_arbitrarily_deep_local_chain() {
        let mut insns = vec![aload(0), string(), check_parameter()];
        for slot in 1..=10u16 {
            insns.push(aload(slot - 1));
            insns.push(astore(slot));
        }
        let removed_from = insns.len() + 1;
        insns.extend([
            aload(10),
            op(DUP),
            string(),
            check_with_message(),
            op(ARETURN),
        ]);
        assert_eq!(
            run_with_handlers_and_locals(insns, &[], 11),
            vec![removed_from, removed_from + 1, removed_from + 2]
        );
    }

    #[test]
    fn matching_non_null_branch_stores_keep_the_fact_at_the_merge() {
        let insns = vec![
            string(),
            astore(1),
            aload(0),
            jump(IFNULL, 7),
            string(),
            astore(1),
            jump(GOTO, 9), // goto merge
            string(),
            astore(1),
            aload(1),
            op(DUP),
            string(),
            check_with_message(),
            op(ARETURN),
        ];
        assert_eq!(run(insns), vec![10, 11, 12]);
    }

    #[test]
    fn a_local_is_non_null_past_its_ifnull_only() {
        // 0 aload_0; 1 ifnull 6; 2 aload_0; 3 dup; 4 invokestatic checkNotNull(Object);
        // 5 areturn; 6 aload_0; 7 dup; 8 invokestatic checkNotNull(Object); 9 areturn
        let insns = vec![
            aload(0),
            jump(IFNULL, 6),
            aload(0),
            op(DUP),
            check(),
            op(ARETURN),
            aload(0),
            op(DUP),
            check(),
            op(ARETURN),
        ];
        assert_eq!(run(insns), vec![3, 4]);
    }

    #[test]
    fn a_new_string_constant_needs_no_expression_check() {
        // 0 ldc "s"; 1 dup; 2 ldc; 3 checkNotNullExpressionValue; 4 areturn
        let insns = vec![string(), op(DUP), string(), check_expression(), op(ARETURN)];
        assert_eq!(run(insns), vec![1, 2, 3]);
    }

    #[test]
    fn a_throw_helper_still_reaches_its_exception_handler() {
        // Two disjoint protected ranges share one handler. The first reaches it with local 1
        // non-null; the Intrinsics throw reaches it with local 1 null. Omitting the throw's
        // exceptional edge would incorrectly remove the handler's check.
        let possibly_throwing = insn(Insn::Method {
            op: INVOKESTATIC,
            owner: "fixture/Owner".to_string(),
            name: "call".to_string(),
            desc: "()V".to_string(),
            interface: false,
        });
        let insns = vec![
            op(0x03),          // 0 iconst_0
            jump(0x99, 6),     // 1 ifeq null_path
            string(),          // 2 non-null String
            astore(1),         // 3
            jump(GOTO, 9),     // 4 goto first protected call
            op(0x00),          // 5 nop
            op(ACONST_NULL),   // 6 null
            astore(1),         // 7
            jump(GOTO, 11),    // 8 goto throwing helper
            possibly_throwing, // 9 an ordinary possibly-throwing call
            jump(GOTO, 17),    // 10 leave
            throw_npe(),       // 11 Intrinsics throw helper
            jump(GOTO, 17),    // 12 unreachable normal edge
            aload(1),          // 13 handler
            op(DUP),           // 14
            check(),           // 15 checkNotNull(Object)
            op(ARETURN),       // 16
            op(0xb1),          // 17 return
        ];
        assert_eq!(
            run_with_handlers(insns, &[(9, 10, 13), (11, 12, 13)]),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn a_label_only_a_line_names_does_not_break_the_straight_line() {
        // 0 ldc "s"; [line] 1 dup; 2 ldc; 3 checkNotNullExpressionValue; 4 areturn
        let mut method = MethodNode::new(0x0009, "f", "()V");
        method.max_locals = 1;
        let line = method.new_label();
        let ldc = |value: &str| Node::Insn(Insn::Ldc(Constant::String(value.into())));
        let I::Insn(check) = check_expression() else {
            unreachable!("an instruction");
        };
        method.nodes = vec![
            ldc("s"),
            Node::Label(line),
            Node::Line {
                line: 2,
                start: line,
            },
            Node::Insn(Insn::Op(DUP)),
            ldc("m"),
            Node::Insn(check),
            Node::Insn(Insn::Op(ARETURN)),
        ];
        assert_eq!(select(&method), vec![3, 4, 5]);
    }

    #[test]
    fn nullness_analysis_has_a_checked_size_ceiling() {
        assert!(analysis_within_limit(1_000, 50));
        assert!(!analysis_within_limit(usize::MAX, 2));
        assert!(!analysis_within_limit(65_535, 65_535));
    }
}
