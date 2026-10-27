//! The decoded form of a JVM method body: basic blocks over typed values.
//!
//! Every operand-stack slot of the class-file body becomes a [`ValueId`], defined once by the
//! operation that pushed it or, for a value live on the stack at a block's entry, by one of that
//! block's [`Block::parameters`]. Stack manipulations (`dup*`, `swap`, `pop*`) leave no operation:
//! they only rearrange which values later operations consume. Local-variable slots stay explicit
//! [`Local`]s, read and written by [`OpKind::LoadLocal`] and [`OpKind::StoreLocal`], so a reused
//! slot, a loop counter or a handler that reads a local needs no join of its own.
//!
//! Every member an operation names keeps its exact JVM owner, name and descriptor, and every
//! instruction of a reached block is kept, null checks and kotlinc's inline markers included:
//! deciding what a call means is the normalization that consumes this form, never the decoder.

use crate::jvm::method_node::{Category, Constant, Handle};

/// A value: an index into [`DecodedBody::values`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

/// A basic block: an index into [`DecodedBody::blocks`]. Blocks are numbered in the order their
/// first instruction appears in the class file; the entry block is `BlockId(0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A local variable: an index into [`DecodedBody::locals`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// The JVM verification type of a value, as kotlinc's `StrictBasicValue` keeps it: int-like
/// primitives stay apart where a descriptor names them, and a reference keeps its descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum JvmType {
    Boolean,
    Byte,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    /// A class or array reference by its field descriptor (`Ljava/lang/String;`, `[I`).
    Reference(String),
    /// The `null` constant before it meets a typed reference.
    Null,
}

impl JvmType {
    /// The type a field descriptor names; `None` for `V` or a malformed descriptor.
    pub fn of_descriptor(descriptor: &str) -> Option<JvmType> {
        Some(match descriptor.as_bytes().first()? {
            b'Z' => JvmType::Boolean,
            b'B' => JvmType::Byte,
            b'C' => JvmType::Char,
            b'S' => JvmType::Short,
            b'I' => JvmType::Int,
            b'J' => JvmType::Long,
            b'F' => JvmType::Float,
            b'D' => JvmType::Double,
            b'L' | b'[' => JvmType::Reference(descriptor.to_string()),
            _ => return None,
        })
    }

    /// The local-slot and stack category the value is moved with.
    pub fn category(&self) -> Category {
        match self {
            JvmType::Boolean | JvmType::Byte | JvmType::Char | JvmType::Short | JvmType::Int => {
                Category::Int
            }
            JvmType::Long => Category::Long,
            JvmType::Float => Category::Float,
            JvmType::Double => Category::Double,
            JvmType::Reference(_) | JvmType::Null => Category::Reference,
        }
    }
}

/// Where a value is defined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueDef {
    /// The `index`th entry of `block`'s parameters, bottom of the stack first.
    BlockParameter { block: BlockId, index: u32 },
    /// The result of the `op`th operation of `block`.
    Op { block: BlockId, op: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub ty: JvmType,
    pub def: ValueDef,
}

/// A local-variable slot as one category uses it. A slot javac or kotlinc reuses for a value of
/// another category is a different local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub slot: u16,
    pub category: Category,
}

/// A point in the decoded body: before operation `op` of `block` (`op == ops.len()` is before its
/// terminator), or past the last reached instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    At { block: BlockId, op: u32 },
    End,
}

/// A `LocalVariableTable` entry over the decoded body: `local` is named `name` and declared
/// `descriptor` from `start` up to `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalScope {
    pub local: LocalId,
    pub name: String,
    pub descriptor: String,
    pub start: Anchor,
    pub end: Anchor,
}

/// A method by its exact class-file reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodRef {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
    /// Whether the reference is an `InterfaceMethodref`.
    pub interface: bool,
}

/// A field by its exact class-file reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldRef {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeKind {
    Virtual,
    Special,
    Static,
    Interface,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    UShr,
    And,
    Or,
    Xor,
    /// `lcmp`.
    Compare,
    /// `fcmpl`/`dcmpl`: `-1` when either operand is NaN.
    CompareNanLess,
    /// `fcmpg`/`dcmpg`: `1` when either operand is NaN.
    CompareNanGreater,
}

/// The element kind an array load or store opcode moves. `baload`/`bastore` serve `byte[]` and
/// `boolean[]` alike; the array value's type tells them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayElement {
    Int,
    Long,
    Float,
    Double,
    Reference,
    ByteOrBoolean,
    Char,
    Short,
}

/// One operation of a block. Operands are values; a result, when there is one, is a fresh value.
#[derive(Clone, Debug, PartialEq)]
pub enum OpKind {
    /// `aconst_null`.
    Null,
    /// `iconst`, `lconst`, `fconst`, `dconst`, `bipush`, `sipush` or `ldc`.
    Constant(Constant),
    LoadLocal(LocalId),
    StoreLocal {
        local: LocalId,
        value: ValueId,
    },
    /// `iinc`.
    IncrementLocal {
        local: LocalId,
        delta: i16,
    },
    Binary {
        op: BinaryOp,
        category: Category,
        lhs: ValueId,
        rhs: ValueId,
    },
    Negate {
        category: Category,
        operand: ValueId,
    },
    /// `i2l` … `i2s`.
    Convert {
        from: Category,
        to: JvmType,
        operand: ValueId,
    },
    /// A method call. `arguments` holds the receiver first for every kind but `Static`.
    Invoke {
        kind: InvokeKind,
        target: MethodRef,
        arguments: Vec<ValueId>,
    },
    InvokeDynamic {
        name: String,
        descriptor: String,
        bootstrap: Handle,
        bootstrap_arguments: Vec<Constant>,
        arguments: Vec<ValueId>,
    },
    /// `getstatic` (`object: None`) or `getfield`.
    GetField {
        field: FieldRef,
        object: Option<ValueId>,
    },
    /// `putstatic` (`object: None`) or `putfield`.
    PutField {
        field: FieldRef,
        object: Option<ValueId>,
        value: ValueId,
    },
    /// `new`: an uninitialized instance of `class`, initialized by a later `invokespecial <init>`
    /// whose receiver is this value.
    New {
        class: String,
    },
    /// `newarray` or `anewarray`, with the element's field descriptor.
    NewArray {
        element: String,
        length: ValueId,
    },
    MultiNewArray {
        descriptor: String,
        dimensions: Vec<ValueId>,
    },
    ArrayLength(ValueId),
    ArrayLoad {
        element: ArrayElement,
        array: ValueId,
        index: ValueId,
    },
    ArrayStore {
        element: ArrayElement,
        array: ValueId,
        index: ValueId,
        value: ValueId,
    },
    /// `checkcast` to an internal name or array descriptor.
    CheckCast {
        class: String,
        value: ValueId,
    },
    InstanceOf {
        class: String,
        value: ValueId,
    },
    MonitorEnter(ValueId),
    MonitorExit(ValueId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub kind: OpKind,
    pub result: Option<ValueId>,
    /// The source line in effect at the instruction, as the `LineNumberTable` assigns it.
    pub line: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Eq,
    Ne,
    Lt,
    Ge,
    Gt,
    Le,
}

/// The condition a conditional jump takes its branch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    /// `ifeq` … `ifle`: `value` compared with zero.
    IntZero {
        comparison: Comparison,
        value: ValueId,
    },
    /// `if_icmpeq` … `if_icmple`.
    Ints {
        comparison: Comparison,
        lhs: ValueId,
        rhs: ValueId,
    },
    /// `if_acmpeq` (`equal: true`) or `if_acmpne`.
    References {
        equal: bool,
        lhs: ValueId,
        rhs: ValueId,
    },
    /// `ifnull` (`is_null: true`) or `ifnonnull`.
    Null { is_null: bool, value: ValueId },
}

/// A control transfer: the target block and, for each of its parameters, the value that flows in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub target: BlockId,
    pub arguments: Vec<ValueId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// A `goto`, or falling through into the next block.
    Goto(Edge),
    Branch {
        condition: Condition,
        taken: Edge,
        not_taken: Edge,
    },
    /// `tableswitch` or `lookupswitch`, as `(key, edge)` cases in the instruction's order.
    Switch {
        value: ValueId,
        cases: Vec<(i32, Edge)>,
        default: Edge,
    },
    Return(Option<ValueId>),
    Throw(ValueId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// The values on the operand stack at entry, bottom first. A handler block has exactly one,
    /// the caught exception.
    pub parameters: Vec<ValueId>,
    pub ops: Vec<Op>,
    pub terminator: Terminator,
    pub terminator_line: Option<u16>,
}

/// An exception-table entry over blocks: an exception of `catch_type` (any throwable for `None`)
/// thrown in one of `covered` transfers to `handler`, with the locals as they stood before the
/// throwing operation. Handlers keep exception-table order, which is their priority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handler {
    pub covered: Vec<BlockId>,
    pub handler: BlockId,
    pub catch_type: Option<String>,
}

/// A decoded method body.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedBody {
    pub owner: String,
    pub name: String,
    pub descriptor: String,
    pub access: u16,
    /// The locals the method's parameters arrive in, the receiver first for an instance method.
    pub parameters: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub values: Vec<Value>,
    /// Every block some path from the entry or a reached handler reaches, in class-file order.
    pub blocks: Vec<Block>,
    pub handlers: Vec<Handler>,
    pub local_scopes: Vec<LocalScope>,
}

impl DecodedBody {
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    pub fn value(&self, id: ValueId) -> &Value {
        &self.values[id.0 as usize]
    }

    pub fn local(&self, id: LocalId) -> Local {
        self.locals[id.0 as usize]
    }
}
