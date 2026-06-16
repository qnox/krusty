//! `krusty-ir` — the backend-agnostic, typed common IR.
//!
//! This is the shared layer between the front end (lex/parse/resolve) and the platform backends
//! (JVM today; WASM/JS future — see `docs/ARCHITECTURE.md`). It deliberately mirrors the **Kotlin
//! IR** node taxonomy (`IrClass`/`IrFunction`/`IrCall`/`IrWhen`/…) rather than inventing a novel
//! design, and it is **not** a low-level IR like LLVM — the JVM/JS/WASM targets are managed VMs that
//! need Kotlin's types, nullability, and object model preserved (which LLVM/MLIR discard too early).
//!
//! Representation choices (primitive vs boxed, erasure, calling conventions) are **not** encoded
//! here — they are decided by each backend's lowering of these nodes. Types are expressed in Kotlin
//! terms (`IrType`), never JVM descriptors.
//!
//! Storage follows krusty's index-based invariant: nodes live in parallel `Vec` arenas keyed by
//! `u32` ids (no `Box`/`Rc` graphs; bulk-freeable). Lowering (`ast → ir`) and the JVM backend
//! consuming IR are the next phases; today this module defines the node set + a builder + a printer.

/// A Kotlin-level type, backend-agnostic. A class is referenced by its **Kotlin FqName**
/// (`kotlin/Int`, `kotlin/String`, a user `foo/Bar`); each backend maps it to a target
/// representation (the JVM backend via the ported `JavaToKotlinClassMap`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrType {
    /// A class/interface by Kotlin FqName, with type arguments (erased or reified per backend).
    Class { fq_name: String, type_args: Vec<IrType>, nullable: bool },
    /// A type-parameter reference (`T`), resolved to its declaration index.
    TypeParameter(u32),
    /// `(P..) -> R` function type — kept structural so backends choose the representation
    /// (JVM `FunctionN`, a JS closure, …).
    Function { params: Vec<IrType>, ret: Box<IrType> },
    /// `kotlin.Unit` / `kotlin.Nothing` — special-cased so control flow needn't synthesize them.
    Unit,
    Nothing,
    /// A dynamically-unknown type (lowering error recovery); backends must not emit it.
    Error,
}

pub type ExprId = u32;
pub type FunId = u32;
pub type ClassId = u32;

/// A compile-time constant (`IrConst` in Kotlin IR).
#[derive(Clone, Debug, PartialEq)]
pub enum IrConst {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Char(char),
    String(String),
    Null,
}

/// An IR expression node (a subset of Kotlin IR's `IrExpression` hierarchy). Operands reference
/// other expressions by `ExprId` into the arena.
#[derive(Clone, Debug)]
pub enum IrExpr {
    Const(IrConst),
    /// Read a value parameter / variable by its declaration index.
    GetValue(u32),
    /// Assign to a variable (`IrSetValue`).
    SetValue { var: u32, value: ExprId },
    /// A call to a function/constructor/operator (`IrCall`) — `callee` resolved at lowering.
    Call { callee: FunId, dispatch_receiver: Option<ExprId>, args: Vec<ExprId> },
    /// `IrReturn` from the enclosing function.
    Return(Option<ExprId>),
    /// `IrBlock` — a sequence of statements; value is the last expression (or Unit).
    Block { stmts: Vec<ExprId>, value: Option<ExprId> },
    /// `IrWhen` — branches of (condition → result); the AST `if`/`when` lower here. `else` is the
    /// branch with a `None` condition.
    When { branches: Vec<(Option<ExprId>, ExprId)> },
    /// `IrTypeOperatorCall` — `is`/`!is`/`as`/`as?`/implicit casts/coercions.
    TypeOp { op: IrTypeOp, arg: ExprId, type_operand: IrType },
    /// `IrWhile` loop.
    While { cond: ExprId, body: ExprId },
    /// A local variable declaration (`IrVariable`), value optional (`lateinit`).
    Variable { index: u32, ty: IrType, init: Option<ExprId> },
    /// A built-in primitive binary operator (`+`/`-`/`<`/`==`/…). Kotlin IR models these as
    /// `IrCall` to the operator function; krusty keeps a primitive node for the built-in numeric and
    /// boolean ops so each backend emits the native instruction (JVM `iadd`, JS `+`).
    PrimitiveBinOp { op: IrBinOp, lhs: ExprId, rhs: ExprId },
}

/// Built-in binary operators carried by `IrExpr::PrimitiveBinOp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add, Sub, Mul, Div, Rem,
    Lt, Le, Gt, Ge, Eq, Ne,
    And, Or,
}

/// The `IrTypeOperatorCall` operators (Kotlin IR's `IrTypeOperator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrTypeOp {
    InstanceOf,    // `is T`
    NotInstanceOf, // `!is T`
    Cast,          // `as T`
    SafeCast,      // `as? T`
    /// Representation coercion the backend inserts (e.g. JVM box/unbox) — explicit in the IR so it
    /// is visible and testable, not hidden in codegen.
    ImplicitCoercion,
}

/// A function/method declaration (`IrFunction`).
#[derive(Clone, Debug)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<IrType>,
    pub ret: IrType,
    /// The body expression (typically an `IrBlock`), or `None` for abstract/external.
    pub body: Option<ExprId>,
    pub is_static: bool,
}

/// A class/interface/object declaration (`IrClass`).
#[derive(Clone, Debug)]
pub struct IrClass {
    pub fq_name: String,
    pub supertypes: Vec<IrType>,
    pub functions: Vec<FunId>,
    pub is_interface: bool,
}

/// One lowered source file (`IrFile`) — its arenas. Index-based, bulk-freeable.
#[derive(Default)]
pub struct IrFile {
    pub package: Option<String>,
    pub functions: Vec<IrFunction>,
    pub classes: Vec<IrClass>,
    pub exprs: Vec<IrExpr>,
}

impl IrFile {
    pub fn expr(&self, id: ExprId) -> &IrExpr {
        &self.exprs[id as usize]
    }
    pub fn add_expr(&mut self, e: IrExpr) -> ExprId {
        let id = self.exprs.len() as u32;
        self.exprs.push(e);
        id
    }
    pub fn add_fun(&mut self, f: IrFunction) -> FunId {
        let id = self.functions.len() as u32;
        self.functions.push(f);
        id
    }
    pub fn add_class(&mut self, c: IrClass) -> ClassId {
        let id = self.classes.len() as u32;
        self.classes.push(c);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_trivial_function_ir() {
        // Model `fun answer(): Int = 42` in the IR by hand (lowering comes in a later phase).
        let mut f = IrFile::default();
        let lit = f.add_expr(IrExpr::Const(IrConst::Int(42)));
        let ret = f.add_expr(IrExpr::Return(Some(lit)));
        let body = f.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
        let fun = f.add_fun(IrFunction {
            name: "answer".to_string(),
            params: vec![],
            ret: IrType::Class { fq_name: "kotlin/Int".to_string(), type_args: vec![], nullable: false },
            body: Some(body),
            is_static: true,
        });
        assert_eq!(f.functions[fun as usize].name, "answer");
        // The return type is a Kotlin FqName, not a JVM descriptor — the backend maps it.
        match &f.functions[fun as usize].ret {
            IrType::Class { fq_name, .. } => assert_eq!(fq_name, "kotlin/Int"),
            other => panic!("expected class type, got {other:?}"),
        }
        assert!(matches!(f.expr(body), IrExpr::Block { .. }));
    }
}
