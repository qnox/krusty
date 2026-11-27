//! The source operators an expression can be written with, and their spellings.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    RefEq,
    RefNe, // === and !==
}

impl BinOp {
    /// The Kotlin operator-function name an arithmetic operator desugars to (`a + b` → `a.plus(b)`),
    /// or `None` for a non-arithmetic operator. The single source of truth shared by the checker and
    /// the lowerer when resolving a user/library `operator fun`.
    pub fn arith_operator_name(self) -> Option<&'static str> {
        Some(match self {
            BinOp::Add => "plus",
            BinOp::Sub => "minus",
            BinOp::Mul => "times",
            BinOp::Div => "div",
            BinOp::Rem => "rem",
            _ => return None,
        })
    }

    /// Inverse of [`arith_operator_name`](Self::arith_operator_name): the arithmetic operator a
    /// Kotlin operator-function name (`plus`/`minus`/…) desugars from, or `None`.
    pub fn from_arith_operator_name(name: &str) -> Option<BinOp> {
        Some(match name {
            "plus" => BinOp::Add,
            "minus" => BinOp::Sub,
            "times" => BinOp::Mul,
            "div" => BinOp::Div,
            "rem" => BinOp::Rem,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,
    Not,
    Plus,
}

impl UnOp {
    pub fn operator_name(self) -> &'static str {
        match self {
            UnOp::Neg => "unaryMinus",
            UnOp::Plus => "unaryPlus",
            UnOp::Not => "not",
        }
    }
}

/// The spelling of a binary operator in the debug tree.
pub(super) fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::RefEq => "===",
        BinOp::RefNe => "!==",
    }
}

/// The spelling of a unary operator in the debug tree.
pub(super) fn unop(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
        UnOp::Plus => "plus",
    }
}
