//! kotlinc's `ReifiedTypeInliner` for the `is`, `as` and `as?` markers of a reified argument.
//!
//! A reified inline body compiles `v is T` to a marker and an `instanceof` of the erased bound, and
//! `v as T` or `v as? T` to a marker and a `checkcast`. The call site's argument decides the real
//! operation, which can need code of its own: a nullable `is` accepts `null`, a non-null `as`
//! rejects it, an `as?` tests before it casts, and a mutable collection or a function type goes
//! through `TypeIntrinsics`.

use crate::ir::TypeCheckRole;
use crate::jvm::method_node::{Constant, Insn, MethodNode, Node};
use crate::jvm::type_intrinsics::{self, CastCheck, InstanceCheck, IntrinsicCall};

/// kotlinc's `ReifiedTypeInliner.OperationKind` values for the operations handled here.
const AS: i32 = 1;
const SAFE_AS: i32 = 2;
const IS: i32 = 3;

const INSTANCEOF: u8 = 0xc1;
const CHECKCAST: u8 = 0xc0;

/// The call site's reified argument, as the marker's operation sees it.
pub(super) struct ReifiedTarget<'a> {
    /// The JVM class the type-bearing instruction names.
    pub(super) class: &'a str,
    /// Whether the substituted type is nullable (the argument's own `?` or the marker's `T?`).
    pub(super) nullable: bool,
    pub(super) intrinsic: Option<TypeCheckRole>,
    /// The substituted type as kotlinc renders it in `null cannot be cast to non-null type …`.
    pub(super) rendered: &'a str,
}

/// Whether marker operation `mode` over the type-bearing instruction `stub` needs code of its own:
/// a nullable `is` accepts `null`, a non-null `as` rejects it, an `as?` tests first, and a
/// `TypeIntrinsics` target calls it. Every other operation only repoints `stub`.
pub(super) fn writes_code(mode: i32, stub: &Node, nullable: bool, intrinsic: bool) -> bool {
    let Node::Insn(Insn::Type { op, .. }) = stub else {
        return false;
    };
    match (mode, *op) {
        (IS, INSTANCEOF) => nullable || intrinsic,
        (AS, CHECKCAST) => !nullable || intrinsic,
        (SAFE_AS, CHECKCAST) => true,
        _ => false,
    }
}

/// The nodes that replace the marker's type-bearing instruction `stub`, or `None` when repointing
/// that instruction at `target.class` is the whole operation.
pub(super) fn expand(
    node: &mut MethodNode,
    mode: i32,
    stub: &Node,
    target: &ReifiedTarget<'_>,
) -> Option<Vec<Node>> {
    let Node::Insn(Insn::Type { op, .. }) = stub else {
        return None;
    };
    if !writes_code(mode, stub, target.nullable, target.intrinsic.is_some()) {
        return None;
    }
    match (mode, *op) {
        (IS, INSTANCEOF) if target.nullable => Some(nullable_instance_check(node, target)),
        (IS, INSTANCEOF) => Some(instance_check(node, target)),
        (AS, CHECKCAST) if !target.nullable || target.intrinsic.is_some() => {
            let mut nodes = Vec::new();
            if !target.nullable {
                nodes.extend(null_check_for_non_safe_as(node, target.rendered));
            }
            nodes.extend(cast(target));
            Some(nodes)
        }
        (SAFE_AS, CHECKCAST) => Some(safe_cast(node, target)),
        _ => None,
    }
}

/// `TypeIntrinsics.instanceOf`: the instance test, leaving an `int` 0/1.
fn instance_check(node: &mut MethodNode, target: &ReifiedTarget<'_>) -> Vec<Node> {
    match target.intrinsic.map(type_intrinsics::instance_check) {
        Some(InstanceCheck::Call(call)) => intrinsic_call(&call),
        Some(InstanceCheck::SuspendFunction { arity }) => {
            suspend_function_instance_check(node, arity)
        }
        None => vec![Node::Insn(Insn::Type {
            op: INSTANCEOF,
            class: target.class.to_owned(),
        })],
    }
}

/// `dup; instanceof SuspendFunction; ifeq fail; isFunctionOfArity; goto end; fail: pop; iconst_0`.
fn suspend_function_instance_check(node: &mut MethodNode, arity: u8) -> Vec<Node> {
    let fail = node.new_label();
    let end = node.new_label();
    let mut nodes = vec![
        Node::Insn(Insn::Op(DUP)),
        Node::Insn(Insn::Type {
            op: INSTANCEOF,
            class: type_intrinsics::SUSPEND_FUNCTION_MARKER.to_owned(),
        }),
        Node::Insn(Insn::Jump {
            op: IFEQ,
            target: fail,
        }),
    ];
    nodes.extend(intrinsic_call(&type_intrinsics::function_arity_check(
        arity,
    )));
    nodes.extend([
        Node::Insn(Insn::Jump {
            op: GOTO,
            target: end,
        }),
        Node::Label(fail),
        Node::Insn(Insn::Op(POP)),
        Node::Insn(Insn::Op(ICONST_0)),
        Node::Label(end),
    ]);
    nodes
}

/// `TypeIntrinsics.checkcast` for a non-safe cast.
fn cast(target: &ReifiedTarget<'_>) -> Vec<Node> {
    let checkcast = Node::Insn(Insn::Type {
        op: CHECKCAST,
        class: target.class.to_owned(),
    });
    match target.intrinsic.map(type_intrinsics::cast) {
        Some(CastCheck::Call {
            call,
            checkcast: keeps_checkcast,
        }) => {
            let mut nodes = intrinsic_call(&call);
            if keeps_checkcast {
                nodes.push(checkcast);
            }
            nodes
        }
        Some(CastCheck::SuspendFunction { arity }) => vec![Node::Insn(Insn::Type {
            op: CHECKCAST,
            class: crate::jvm::names::function_interface_internal_name(usize::from(arity)),
        })],
        None => vec![checkcast],
    }
}

/// `generateIsCheck` for a nullable type: `null` is an instance, so it is accepted before the
/// instance test sees it.
fn nullable_instance_check(node: &mut MethodNode, target: &ReifiedTarget<'_>) -> Vec<Node> {
    let null = node.new_label();
    let end = node.new_label();
    let mut nodes = vec![
        Node::Insn(Insn::Op(DUP)),
        Node::Insn(Insn::Jump {
            op: IFNULL,
            target: null,
        }),
    ];
    nodes.extend(instance_check(node, target));
    nodes.extend([
        Node::Insn(Insn::Jump {
            op: GOTO,
            target: end,
        }),
        Node::Label(null),
        Node::Insn(Insn::Op(POP)),
        Node::Insn(Insn::Op(ICONST_1)),
        Node::Label(end),
    ]);
    nodes
}

/// `generateNullCheckForNonSafeAs`: a `null` operand throws before the cast.
fn null_check_for_non_safe_as(node: &mut MethodNode, rendered: &str) -> Vec<Node> {
    let checked = node.new_label();
    vec![
        Node::Insn(Insn::Op(DUP)),
        Node::Insn(Insn::Jump {
            op: IFNONNULL,
            target: checked,
        }),
        Node::Insn(Insn::Type {
            op: NEW,
            class: NULL_POINTER_EXCEPTION.to_owned(),
        }),
        Node::Insn(Insn::Op(DUP)),
        Node::Insn(Insn::Ldc(Constant::String(
            format!("null cannot be cast to non-null type {rendered}").into(),
        ))),
        Node::Insn(Insn::Method {
            op: INVOKESPECIAL,
            owner: NULL_POINTER_EXCEPTION.to_owned(),
            name: "<init>".to_owned(),
            desc: "(Ljava/lang/String;)V".to_owned(),
            interface: false,
        }),
        Node::Insn(Insn::Op(ATHROW)),
        Node::Label(checked),
    ]
}

/// `generateAsCast` for `as?`: a value that fails the instance test becomes `null`, and the cast
/// that follows is a plain `checkcast`.
fn safe_cast(node: &mut MethodNode, target: &ReifiedTarget<'_>) -> Vec<Node> {
    let accepted = node.new_label();
    let mut nodes = vec![Node::Insn(Insn::Op(DUP))];
    nodes.extend(instance_check(node, target));
    nodes.extend([
        Node::Insn(Insn::Jump {
            op: IFNE,
            target: accepted,
        }),
        Node::Insn(Insn::Op(POP)),
        Node::Insn(Insn::Op(ACONST_NULL)),
        Node::Label(accepted),
        Node::Insn(Insn::Type {
            op: CHECKCAST,
            class: target.class.to_owned(),
        }),
    ]);
    nodes
}

fn intrinsic_call(call: &IntrinsicCall) -> Vec<Node> {
    let mut nodes = Vec::new();
    if let Some(arity) = call.arity {
        nodes.push(Node::Insn(push_arity(arity)));
    }
    nodes.push(Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner: call.owner().to_owned(),
        name: call.name.clone(),
        desc: call.descriptor.clone(),
        interface: false,
    }));
    nodes
}

fn push_arity(arity: u8) -> Insn {
    match arity {
        0..=5 => Insn::Op(ICONST_0 + arity),
        6..=127 => Insn::Int {
            op: BIPUSH,
            operand: i32::from(arity),
        },
        _ => Insn::Int {
            op: SIPUSH,
            operand: i32::from(arity),
        },
    }
}

const NULL_POINTER_EXCEPTION: &str = "java/lang/NullPointerException";
const ACONST_NULL: u8 = 0x01;
const ICONST_0: u8 = 0x03;
const ICONST_1: u8 = 0x04;
const BIPUSH: u8 = 0x10;
const SIPUSH: u8 = 0x11;
const POP: u8 = 0x57;
const DUP: u8 = 0x59;
const IFEQ: u8 = 0x99;
const IFNE: u8 = 0x9a;
const GOTO: u8 = 0xa7;
const ATHROW: u8 = 0xbf;
const NEW: u8 = 0xbb;
const INVOKESPECIAL: u8 = 0xb7;
const INVOKESTATIC: u8 = 0xb8;
const IFNULL: u8 = 0xc6;
const IFNONNULL: u8 = 0xc7;
