//! The node types: ASM's tree API reduced to what a method body needs.

use crate::kt_string::KtString;

/// A position in a [`MethodNode`]'s instruction list. It is placed by a [`Node::Label`] and named
/// by jumps, switches, line numbers, try/catch ranges and local-variable ranges. Ids are only
/// meaningful within their own node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LabelId(pub(super) u32);

impl LabelId {
    /// The label's position among its method's labels, `0..MethodNode::label_count()`: a key for
    /// tables that follow a body's labels.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One entry of the instruction list.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// Places `LabelId` before the next instruction.
    Label(LabelId),
    /// `line` starts at `start` (ASM's `LineNumberNode`, which follows its label).
    Line {
        line: u16,
        start: LabelId,
    },
    Insn(Insn),
}

/// An instruction with every constant-pool operand resolved to its symbolic form and every branch
/// target to a label. Short and wide encodings are not distinguished: `iload_1`, `iload 1` and
/// `wide iload 1` are all `Var { op: ILOAD, slot: 1 }`, `ldc`/`ldc_w`/`ldc2_w` are all `Ldc`, and
/// `goto_w`/`jsr_w` are `Jump` with `goto`/`jsr`. The assembler picks the encoding.
#[derive(Clone, Debug, PartialEq)]
pub enum Insn {
    /// An instruction with no operand (`iadd`, `areturn`, `dup`, …).
    Op(u8),
    /// `bipush`, `sipush` or `newarray` with its immediate operand.
    Int {
        op: u8,
        operand: i32,
    },
    /// A local load or store (`iload`…`astore`, in their long-form opcodes) or `ret`.
    Var {
        op: u8,
        slot: u16,
    },
    Iinc {
        slot: u16,
        delta: i16,
    },
    /// `new`, `anewarray`, `checkcast` or `instanceof`; `class` is an internal name or, for an
    /// array class, its descriptor.
    Type {
        op: u8,
        class: String,
    },
    Field {
        op: u8,
        owner: String,
        name: String,
        desc: String,
    },
    Method {
        op: u8,
        owner: String,
        name: String,
        desc: String,
        /// Whether `owner` is an interface (`InterfaceMethodref`).
        interface: bool,
    },
    InvokeDynamic {
        name: String,
        desc: String,
        bootstrap: Handle,
        arguments: Vec<Constant>,
    },
    /// A conditional branch, `goto` or `jsr`.
    Jump {
        op: u8,
        target: LabelId,
    },
    Ldc(Constant),
    TableSwitch {
        low: i32,
        high: i32,
        default: LabelId,
        labels: Vec<LabelId>,
    },
    LookupSwitch {
        default: LabelId,
        keys: Vec<i32>,
        labels: Vec<LabelId>,
    },
    MultiANewArray {
        desc: String,
        dims: u8,
    },
}

/// A loadable constant: an `ldc` operand or a bootstrap-method argument.
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Int(i32),
    /// Raw IEEE bits, so that NaN payloads and `-0.0` survive and constants compare exactly.
    Float(u32),
    Long(i64),
    Double(u64),
    String(KtString),
    /// A class constant: an internal name or an array descriptor.
    Class(String),
    MethodType(String),
    Handle(Handle),
}

impl Constant {
    /// Whether the constant takes two stack words (`ldc2_w`).
    pub fn is_wide(&self) -> bool {
        matches!(self, Constant::Long(_) | Constant::Double(_))
    }
}

/// A `CONSTANT_MethodHandle`: a reference kind (JVMS 4.4.8) onto a field or method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handle {
    pub kind: u8,
    pub owner: String,
    pub name: String,
    pub desc: String,
    /// Whether the member is named through an `InterfaceMethodref`.
    pub interface: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TryCatchBlock {
    pub start: LabelId,
    pub end: LabelId,
    pub handler: LabelId,
    /// The caught class, or `None` for a catch-all (`finally`) handler.
    pub catch_type: Option<String>,
}

/// A `LocalVariableTable` entry: `name` of type `desc` lives in `slot` over `[start, end)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalVariable {
    pub name: String,
    pub desc: String,
    pub start: LabelId,
    pub end: LabelId,
    pub slot: u16,
}

/// A method: its identity and its body as a node list plus the tables that refer into it.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodNode {
    pub access: u16,
    pub name: String,
    pub desc: String,
    pub nodes: Vec<Node>,
    /// In exception-table order, which is also handler priority.
    pub try_catch_blocks: Vec<TryCatchBlock>,
    pub local_variables: Vec<LocalVariable>,
    pub max_stack: u16,
    pub max_locals: u16,
    pub(super) label_count: u32,
}

impl MethodNode {
    /// An empty body, the start of a node built from scratch rather than read.
    pub fn new(access: u16, name: &str, desc: &str) -> MethodNode {
        MethodNode {
            access,
            name: name.to_string(),
            desc: desc.to_string(),
            nodes: Vec::new(),
            try_catch_blocks: Vec::new(),
            local_variables: Vec::new(),
            max_stack: 0,
            max_locals: 0,
            label_count: 0,
        }
    }

    /// A label no node of this method names yet.
    pub fn new_label(&mut self) -> LabelId {
        let label = LabelId(self.label_count);
        self.label_count += 1;
        label
    }

    /// How many labels this method has handed out; every [`LabelId::index`] is below it.
    pub fn label_count(&self) -> usize {
        self.label_count as usize
    }

    /// The instructions alone, in order.
    pub fn instructions(&self) -> impl Iterator<Item = &Insn> {
        self.nodes.iter().filter_map(|node| match node {
            Node::Insn(insn) => Some(insn),
            _ => None,
        })
    }
}
