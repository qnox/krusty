//! kotlinc's `NullabilityAssumptionsBuilder`: before the nullability analysis, the transformer
//! writes into a copy of the method what each check of a local teaches about it, so the analysis
//! carries the fact to the code after the check.
//!
//! A check depends on a local when the node before it loads the local (`aload`), or duplicates the
//! load right before (`aload; dup`). Past it:
//!
//! - `ifnull`/`ifnonnull`: the jump is retargeted to a new label appended to the method; on the
//!   `null` edge `aconst_null; astore` makes the local `null`, on the other edge
//!   `aload; AS_NOT_NULL; astore` makes it non-null, and whichever of the two sits at the new label
//!   jumps on to the original target;
//! - `instanceof` followed by `ifeq`/`ifne`: the local is non-null on the edge where the test held;
//! - a `checkNotNull*` call (or `checkNotNullParameter`): the local is non-null after it.
//!
//! After each call of an `Intrinsics` throw helper `aconst_null; athrow` marks the rest of the path
//! dead. The facts only feed the analysis: the transformer rewrites the method as it was.
//!
//! "The node before" is ASM's: a line number is a node, and a label nothing refers to is none.

use std::collections::{BTreeSet, HashMap};

use super::super::analysis::opcode;
use super::super::opcodes::*;
use super::nullability::Placements;
use super::{is_throw_intrinsic, Check};
use crate::jvm::method_node::{Constant, Insn, LabelId, MethodNode, Node};

/// The owner of kotlinc's pseudo instructions: a call to it never reaches the class file.
const PSEUDO_INSN_OWNER: &str = "kotlin/jvm/internal/$$$$$NON_EXISTENT_CLASS";
/// `PseudoInsn.AS_NOT_NULL`'s signature.
const AS_NOT_NULL_DESC: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";

/// `ReifiedTypeInliner.OperationKind.SAFE_AS`.
const SAFE_AS: i32 = 2;
/// `ReifiedTypeInliner.OperationKind.TYPE_OF`.
const TYPE_OF: i32 = 6;

/// The nodes ASM's list holds, with each one's neighbours in it.
pub(super) struct Listing {
    referenced: BTreeSet<LabelId>,
}

impl Listing {
    pub(super) fn of(method: &MethodNode) -> Listing {
        Listing {
            referenced: method.referenced_labels(),
        }
    }

    /// Whether ASM's list holds the node: a label only when something refers to it.
    pub(super) fn holds(&self, node: &Node) -> bool {
        !matches!(node, Node::Label(label) if !self.referenced.contains(label))
    }

    /// `getPrevious` of the node at `at`.
    pub(super) fn previous(&self, nodes: &[Node], at: usize) -> Option<usize> {
        (0..at).rev().find(|&index| self.holds(&nodes[index]))
    }

    /// `getNext` of the node at `at`.
    fn next(&self, nodes: &[Node], at: usize) -> Option<usize> {
        (at + 1..nodes.len()).find(|&index| self.holds(&nodes[index]))
    }
}

fn insn_at(nodes: &[Node], at: Option<usize>) -> Option<&Insn> {
    match nodes.get(at?)? {
        Node::Insn(insn) => Some(insn),
        _ => None,
    }
}

fn op_at(nodes: &[Node], at: Option<usize>) -> Option<u8> {
    insn_at(nodes, at).map(opcode)
}

fn aload_slot(nodes: &[Node], at: Option<usize>) -> Option<u16> {
    match insn_at(nodes, at)? {
        Insn::Var { op: ALOAD, slot } => Some(*slot),
        _ => None,
    }
}

/// kotlinc's `getIntConstant`.
fn int_constant(insn: &Insn) -> Option<i32> {
    match insn {
        Insn::Op(op @ ICONST_M1..=ICONST_5) => Some(i32::from(*op) - i32::from(ICONST_0)),
        Insn::Int {
            op: BIPUSH | SIPUSH,
            operand,
        } => Some(*operand),
        Insn::Ldc(Constant::Int(value)) => Some(*value),
        _ => None,
    }
}

/// Whether `insn` is `Intrinsics.reifiedOperationMarker`.
pub(super) fn is_reified_marker(insn: &Insn) -> bool {
    matches!(insn, Insn::Method { op: INVOKESTATIC, owner, name, .. }
        if owner == "kotlin/jvm/internal/Intrinsics" && name == "reifiedOperationMarker")
}

/// The operation kind of the reified-operation marker right before the node at `at`
/// (`isOperationReifiedMarker` and `getOperationKind`).
fn marker_kind_before(listing: &Listing, nodes: &[Node], at: usize) -> Option<i32> {
    let marker = listing.previous(nodes, at)?;
    if !insn_at(nodes, Some(marker)).is_some_and(is_reified_marker) {
        return None;
    }
    let name = listing.previous(nodes, marker)?;
    let kind = listing.previous(nodes, name)?;
    insn_at(nodes, Some(kind)).and_then(int_constant)
}

/// Where the reified-operation markers change what an instruction means.
pub(super) fn placements(listing: &Listing, method: &MethodNode) -> Placements {
    let mut placements = Placements::default();
    for (at, node) in method.nodes.iter().enumerate() {
        let Node::Insn(insn) = node else {
            continue;
        };
        match opcode(insn) {
            ACONST_NULL if marker_kind_before(listing, &method.nodes, at) == Some(TYPE_OF) => {
                placements.type_of_placeholders.insert(at);
            }
            CHECKCAST if marker_kind_before(listing, &method.nodes, at) == Some(SAFE_AS) => {
                placements.reified_safe_casts.insert(at);
            }
            _ => {}
        }
    }
    placements
}

/// The local a check at `at` depends on (`collectVariableDependentChecks`).
fn checked_local(listing: &Listing, nodes: &[Node], at: usize, insn: &Insn) -> Option<u16> {
    let previous = |index: usize| listing.previous(nodes, index);
    let load_or_dup_load = |index: Option<usize>| match op_at(nodes, index) {
        Some(ALOAD) => aload_slot(nodes, index),
        Some(DUP) => aload_slot(nodes, previous(index?)),
        _ => None,
    };
    let before = previous(at);
    if matches!(opcode(insn), INSTANCEOF | IFNULL | IFNONNULL) {
        return load_or_dup_load(before);
    }
    match Check::of(insn)? {
        Check::NotNull => load_or_dup_load(before),
        Check::NotNullWithMessage => {
            (op_at(nodes, before) == Some(LDC)).then_some(())?;
            load_or_dup_load(previous(before?))
        }
        Check::Parameter => {
            (op_at(nodes, before) == Some(LDC)).then_some(())?;
            aload_slot(nodes, previous(before?))
        }
        Check::ExpressionValue => {
            (op_at(nodes, before) == Some(LDC)).then_some(())?;
            load_or_dup_load(previous(before?))
        }
    }
}

/// The locals the checks depend on, each with its checks in instruction order, in the order
/// kotlinc's `HashMap<Int, List>` visits them: by bucket (`slot & (capacity - 1)`), then by first
/// insertion.
fn checks_by_local(listing: &Listing, method: &MethodNode) -> Vec<(u16, Vec<usize>)> {
    let mut locals: Vec<(u16, Vec<usize>)> = Vec::new();
    let mut index: HashMap<u16, usize> = HashMap::new();
    for (at, node) in method.nodes.iter().enumerate() {
        let Node::Insn(insn) = node else {
            continue;
        };
        let Some(slot) = checked_local(listing, &method.nodes, at, insn) else {
            continue;
        };
        let entry = *index.entry(slot).or_insert_with(|| {
            locals.push((slot, Vec::new()));
            locals.len() - 1
        });
        locals[entry].1.push(at);
    }
    // `HashMap`'s table doubles past three quarters full, from 16 buckets.
    let mut capacity = 16usize;
    while locals.len() * 4 > capacity * 3 {
        capacity *= 2;
    }
    locals.sort_by_key(|(slot, _)| usize::from(*slot) & (capacity - 1));
    locals
}

/// The method with the assumptions written in, and where each of its original nodes went.
pub(super) struct Assumed {
    pub(super) method: MethodNode,
    /// The node index in `method` of each node of the original method.
    pub(super) moved_to: Vec<usize>,
    pub(super) placements: Placements,
}

/// What the builder writes: code after an original node, and code appended to the method.
#[derive(Default)]
struct Injection {
    after: HashMap<usize, Vec<Node>>,
    retarget: HashMap<usize, LabelId>,
    tail: Vec<Node>,
}

impl Injection {
    /// `InsnList.insert(after, code)`: the code goes right after the node, ahead of anything
    /// inserted there before.
    fn insert_after(&mut self, at: usize, code: Vec<Node>) {
        let slot = self.after.entry(at).or_default();
        slot.splice(0..0, code);
    }
}

fn insn(insn: Insn) -> Node {
    Node::Insn(insn)
}

/// `aload slot; AS_NOT_NULL; astore slot`.
fn assume_not_null(slot: u16) -> Vec<Node> {
    vec![
        insn(Insn::Var { op: ALOAD, slot }),
        insn(Insn::Method {
            op: INVOKESTATIC,
            owner: PSEUDO_INSN_OWNER.to_string(),
            name: "AS_NOT_NULL".to_string(),
            desc: AS_NOT_NULL_DESC.to_string(),
            interface: false,
        }),
        insn(Insn::Var { op: ASTORE, slot }),
    ]
}

/// `aconst_null; astore slot`.
fn assume_null(slot: u16) -> Vec<Node> {
    vec![
        insn(Insn::Op(ACONST_NULL)),
        insn(Insn::Var { op: ASTORE, slot }),
    ]
}

fn goto(target: LabelId) -> Node {
    insn(Insn::Jump { op: GOTO, target })
}

/// `injectNullabilityAssumptions`: a copy of `method` with every assumption written in.
pub(super) fn inject(listing: &Listing, method: &MethodNode) -> Assumed {
    let mut assumed = method.clone();
    let mut injection = Injection::default();
    let nodes = &method.nodes;
    for (slot, checks) in checks_by_local(listing, method) {
        for at in checks {
            let Node::Insn(check) = &nodes[at] else {
                continue;
            };
            match check {
                Insn::Jump {
                    op: op @ (IFNULL | IFNONNULL),
                    target,
                } => {
                    let is_null = *op == IFNULL;
                    let new_label = assumed.new_label();
                    injection.retarget.insert(at, new_label);
                    let mut on_null = assume_null(slot);
                    let mut on_not_null = assume_not_null(slot);
                    injection.tail.push(Node::Label(new_label));
                    if is_null {
                        on_null.push(goto(*target));
                        injection.tail.extend(on_null);
                        injection.insert_after(at, on_not_null);
                    } else {
                        on_not_null.push(goto(*target));
                        injection.insert_after(at, on_null);
                        injection.tail.extend(on_not_null);
                    }
                }
                Insn::Type { op: INSTANCEOF, .. } => {
                    let next = listing.next(nodes, at);
                    match insn_at(nodes, next) {
                        Some(Insn::Jump { op: IFNE, target }) => {
                            let new_label = assumed.new_label();
                            let jump = next.expect("a jump follows");
                            injection.retarget.insert(jump, new_label);
                            injection.tail.push(Node::Label(new_label));
                            injection.tail.extend(assume_not_null(slot));
                            injection.tail.push(goto(*target));
                        }
                        Some(Insn::Jump { op: IFEQ, .. }) => {
                            let jump = next.expect("a jump follows");
                            injection.insert_after(jump, assume_not_null(slot));
                        }
                        _ => {}
                    }
                }
                _ => injection.insert_after(at, assume_not_null(slot)),
            }
        }
    }
    for (at, node) in nodes.iter().enumerate() {
        if matches!(node, Node::Insn(insn) if is_throw_intrinsic(insn)) {
            injection.insert_after(
                at,
                vec![insn(Insn::Op(ACONST_NULL)), insn(Insn::Op(ATHROW))],
            );
        }
    }
    let original = placements(listing, method);
    let mut placements = Placements::default();
    let mut moved_to = Vec::with_capacity(nodes.len());
    let mut written = Vec::with_capacity(nodes.len() + injection.tail.len());
    let write = |node: Node, written: &mut Vec<Node>, placements: &mut Placements| {
        if matches!(&node, Node::Insn(Insn::Method { owner, .. }) if owner == PSEUDO_INSN_OWNER) {
            placements.as_not_null.insert(written.len());
        }
        written.push(node);
    };
    for (at, node) in nodes.iter().enumerate() {
        moved_to.push(written.len());
        if original.type_of_placeholders.contains(&at) {
            placements.type_of_placeholders.insert(written.len());
        }
        if original.reified_safe_casts.contains(&at) {
            placements.reified_safe_casts.insert(written.len());
        }
        let node = match (node, injection.retarget.get(&at)) {
            (Node::Insn(Insn::Jump { op, .. }), Some(label)) => insn(Insn::Jump {
                op: *op,
                target: *label,
            }),
            _ => node.clone(),
        };
        write(node, &mut written, &mut placements);
        for code in injection.after.remove(&at).unwrap_or_default() {
            write(code, &mut written, &mut placements);
        }
    }
    for code in injection.tail {
        write(code, &mut written, &mut placements);
    }
    assumed.nodes = written;
    Assumed {
        method: assumed,
        moved_to,
        placements,
    }
}
