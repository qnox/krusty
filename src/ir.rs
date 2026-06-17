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

/// The target of an `IrExpr::Call`. `Local` references a function defined in this IR file;
/// `External` references a symbol that is **not** — a stdlib `expect`/operator named by its Kotlin
/// FqName (`kotlin/Array.size`, `kotlin/String.plus`, `kotlin/collections/listOf`). Each backend
/// resolves an `External` the way kotlinc does: if it is one of the handful in the **intrinsic
/// table** (array access, arithmetic, …) it emits target bytecode directly; otherwise it resolves
/// the platform **`actual`** from the linked stdlib (`kotlin-stdlib-jvm`/`-js`) and emits a normal
/// call. Either way it is *data* (a FqName), never a new IR node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    Local(FunId),
    External(String),
}

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
    /// A call to a function/constructor/operator/stdlib intrinsic (`IrCall`). The `callee` is a
    /// resolved [`Callee`]: a local function, or an intrinsic identified by Kotlin FqName that each
    /// backend maps to its platform (`kotlin/String.plus`, `kotlin/io/println`, …). This single node
    /// expresses every call — there is no dedicated node per stdlib operation.
    Call { callee: Callee, dispatch_receiver: Option<ExprId>, args: Vec<ExprId> },
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
    /// A built-in primitive binary operator (`+`/`-`/`<`/`==`/…) on numeric/boolean operands. One
    /// parameterized node (not one-per-intrinsic): Kotlin IR models these as `IrCall` to the
    /// operator function, but the built-in numeric/boolean ops are universal across backends, so a
    /// single node lets each emit the native instruction (JVM `iadd`, JS `+`). Every *other*
    /// operator/stdlib operation — `String.plus`, `toString`, `println`, collections — is an
    /// ordinary `Call` to a `Callee::External` symbol the backend resolves; there is no per-op node.
    PrimitiveBinOp { op: IrBinOp, lhs: ExprId, rhs: ExprId },
    /// Read an instance field (`IrGetField`): `receiver.<fields[index]>` of class `class`.
    GetField { receiver: ExprId, class: ClassId, index: u32 },
    /// Write an instance field (`IrSetField`): `receiver.<fields[index]> = value` (statement).
    SetField { receiver: ExprId, class: ClassId, index: u32, value: ExprId },
    /// Read a top-level (module) property — `statics[index]`, a static field on the file facade.
    GetStatic(u32),
    /// Write a top-level (module) property — `statics[index] = value` (statement).
    SetStatic { index: u32, value: ExprId },
    /// Construct an instance (`IrConstructorCall`) of `class` with constructor `args` (in field order).
    New { class: ClassId, args: Vec<ExprId> },
    /// A virtual call to a class instance method `methods[index]` of `class` on `receiver`.
    MethodCall { class: ClassId, index: u32, receiver: ExprId, args: Vec<ExprId> },
    /// Read an enum entry constant: `Enum.ENTRY` — `getstatic <class>.<entry>:L<class>;`.
    EnumEntry { class: ClassId, index: u32 },
    /// The singleton instance of an `object` — `getstatic <class>.INSTANCE:L<class>;`.
    ObjectInstance { class: ClassId },
    /// Call a static method of a class (`Enum.values()`, `Enum.valueOf(s)`).
    EnumValues { class: ClassId },
    EnumValueOf { class: ClassId, arg: ExprId },
    /// A lambda literal — emitted as `invokedynamic` + `LambdaMetafactory` producing a
    /// `kotlin/jvm/functions/Function{arity}`. `impl_fn` is the synthesized static method holding the
    /// body; `captures` are the free-variable values bound into the call site (empty = non-capturing).
    Lambda { impl_fn: u32, arity: u8, captures: Vec<ExprId> },
    /// Invoke a function value (`f(args)` where `f: (A,…) -> R`) via the `FunctionN.invoke` interface
    /// method. Arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`.
    InvokeFunction { func: ExprId, args: Vec<ExprId>, ret: IrType },
    /// The not-null assertion `operand!!` — yields `operand`, throwing if it is null. On the JVM this
    /// is `kotlin/jvm/internal/Intrinsics.checkNotNull` applied to a duplicate of the value.
    NotNullAssert { operand: ExprId },
    /// Construct an instance of a classpath (non-IR) class — `RuntimeException("x")`, `StringBuilder()`.
    /// `internal` is the JVM internal name, `ctor_desc` the `(…)V` constructor descriptor.
    NewExternal { internal: String, ctor_desc: String, args: Vec<ExprId> },
    /// `throw operand` — throws the (Throwable) value; control never falls through (`Nothing`).
    Throw { operand: ExprId },
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
    /// `Some(class fq_name)` for an instance method — `this` is value index 0, params follow.
    pub dispatch_receiver: Option<String>,
    /// Per-parameter `Some(name)` when the backend should guard it with a non-null assertion at method
    /// entry (`Intrinsics.checkNotNullParameter` on the JVM) — non-null reference parameters of a
    /// visible (non-private) function. Empty for synthesized methods (no guards). Parallel to `params`.
    pub param_checks: Vec<Option<String>>,
}

/// A class/interface/object declaration (`IrClass`). Instance fields come from the primary
/// constructor's `val`/`var` parameters (in order); the constructor stores each.
#[derive(Clone, Debug)]
pub struct IrClass {
    pub fq_name: String,
    pub supertypes: Vec<IrType>,
    /// Instance fields `(name, type)`. The first `ctor_param_count` are the primary-constructor
    /// parameters (stored directly from args, in order); any after them are class-body properties
    /// initialized by `init_body`.
    pub fields: Vec<(String, IrType)>,
    /// How many leading `fields` are constructor parameters (the rest are body properties).
    pub ctor_param_count: u32,
    /// Constructor body run after the parameter fields are stored: an effect `Block` (body-property
    /// initializers as `SetField`, `init { … }` blocks) lowered with `this` = value 0 and the
    /// constructor parameters as values `1..=ctor_param_count`. `None` when there's nothing to run.
    pub init_body: Option<ExprId>,
    /// Instance methods — `FunId`s into `IrFile.functions` (each with `dispatch_receiver = Some`).
    pub methods: Vec<FunId>,
    pub is_interface: bool,
    /// JVM superclass internal name (`java/lang/Object` normally, `java/lang/Enum` for an enum, or a
    /// user base class for `class B : A(args)`).
    pub superclass: String,
    /// Arguments to the base-class constructor (`: A(args)`) — lowered IR value ids, evaluated with
    /// `this`=value 0 and the primary-constructor params as values `1..=ctor_param_count`. Empty
    /// unless `superclass` is a user base class.
    pub super_args: Vec<ExprId>,
    /// Enum entries in declaration order: `(entry_name, constructor_arg_value_ids)`. Non-empty only
    /// for an `enum class`; the backend emits a static field per entry, a `$VALUES` array, a
    /// `<clinit>` that constructs them, and `values()`/`valueOf(String)`.
    pub enum_entries: Vec<(String, Vec<ExprId>)>,
    /// Synthetic bridge methods: an override whose erased signature differs from the supertype's
    /// (a generic/covariant override) needs an `ACC_BRIDGE` method with the supertype's descriptor
    /// that adapts arguments and delegates to the concrete override.
    pub bridges: Vec<Bridge>,
    /// Implemented interface internal names (`class C : I, J`). The class file lists them as
    /// `implements`; an interface declaration lists its super-interfaces here.
    pub interfaces: Vec<String>,
    /// `object Foo` — a singleton: a `public static final Foo INSTANCE` field, a private no-arg
    /// constructor, and a `<clinit>` that constructs the instance.
    pub is_object: bool,
}

/// A synthetic bridge method (`name(erased_params)erased_ret` → `name(concrete_params)concrete_ret`).
#[derive(Clone, Debug)]
pub struct Bridge {
    pub name: String,
    pub erased_params: Vec<IrType>,
    pub erased_ret: IrType,
    pub concrete_params: Vec<IrType>,
    pub concrete_ret: IrType,
}

/// A top-level (module) property: a static field on the file facade, initialized in `<clinit>`.
#[derive(Clone, Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: IrType,
    /// The initializer expression (run in `<clinit>` in declaration order).
    pub init: ExprId,
}

/// One lowered source file (`IrFile`) — its arenas. Index-based, bulk-freeable.
#[derive(Default)]
pub struct IrFile {
    pub package: Option<String>,
    pub functions: Vec<IrFunction>,
    pub classes: Vec<IrClass>,
    /// Top-level properties — static fields on the facade, initialized in `<clinit>` in order.
    pub statics: Vec<IrStatic>,
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
            dispatch_receiver: None,
            param_checks: Vec::new(),
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
