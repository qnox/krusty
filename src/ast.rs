//! Index-based arena AST (data-oriented: no `Box`/`Rc` graph, all edges are `u32` ids into
//! parallel `Vec`s, so a file's whole AST is one bulk-freeable allocation block).

use crate::diag::Span;
use crate::kt_string::{KtString, KtStringBuf};
use crate::types::Visibility;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ExprId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct StmtId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct DeclId(pub u32);

pub fn first_lambda_param_or_it(params: &[String]) -> String {
    params.first().cloned().unwrap_or_else(|| "it".to_string())
}

pub fn lambda_params_or_implicit(params: &[String], arity: usize) -> Option<Vec<String>> {
    if !params.is_empty() {
        Some(params.to_vec())
    } else if arity == 1 {
        Some(vec![first_lambda_param_or_it(params)])
    } else if arity == 0 {
        Some(Vec::new())
    } else {
        None
    }
}

/// A FIELD-LESS `companion object` property (`companion object { val ZERO: T get() = … }`): it IS
/// its accessors, so there is no static to hoist onto the outer class — kotlinc emits only `getX`
/// (plus `setX` for a `var`) on `C$Companion`, and that is what the companion synthesis builds.
///
/// A `var` requires a BODIED setter for the same reason a `val` requires a getter: with no backing
/// field a default setter would have nothing to write. A getter that reads `field`, an initializer,
/// an explicit backing field, a delegate, or `const` all mean a real static exists, so those keep
/// the plain companion-property path.
pub fn is_computed_companion_prop(p: &PropDecl) -> bool {
    p.receiver.is_none()
        && !p.is_lateinit
        && !p.is_const
        && p.init.is_none()
        && p.delegate.is_none()
        && p.explicit_backing_field.is_none()
        && p.getter.is_some()
        && !p.getter_reads_field
        && if p.is_var {
            // A `private set` narrows only the SETTER, and the accessor synthesis emits an
            // unconditionally public `setX` — accepting one would let a write through that kotlinc
            // rejects, so those keep the rejection path until the narrowed visibility is modeled.
            p.setter
                .as_ref()
                .is_some_and(|setter| setter.body.is_some() && !setter.is_private)
        } else {
            p.setter.is_none()
        }
}

pub fn setter_param_or_value(param: Option<&String>) -> String {
    param.cloned().unwrap_or_else(|| "value".to_string())
}

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

#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64),
    LongLit(i64),
    /// Unsigned integer literals (`1u`, `0xFFu`, `1uL`). The value is the unsigned magnitude; the
    /// backend reinterprets it as the signed `int`/`long` bit pattern it is represented by.
    UIntLit(i64),
    ULongLit(i64),
    DoubleLit(f64),
    FloatLit(f32),
    BoolLit(bool),
    /// A `String` literal, held as its UTF-16 code UNITS (see [`KtString`]). Not a Rust `String`:
    /// `"\uD800"` and `"\uD83D\uDE00"` denote surrogate code units, which no `String` can hold.
    StringLit(KtString),
    /// A `Char` literal, held as its UTF-16 code UNIT (see `IrConst::Char`). Not a Rust `char`: a
    /// lone surrogate (`'\uD800'`) is a legal Kotlin `Char` but not a legal Unicode scalar value.
    CharLit(u16),
    NullLit,
    Name(String),
    /// `operand!!` — not-null assertion (throws NPE if null, else the value).
    NotNull {
        operand: ExprId,
    },
    /// `lhs ?: rhs` — Elvis (lhs if non-null, else rhs).
    Elvis {
        lhs: ExprId,
        rhs: ExprId,
    },
    /// A string template `"a${e}b$c"` — alternating literal and interpolated-expression parts.
    Template(Vec<TemplatePart>),
    /// `receiver?.name` (args `None`) or `receiver?.name(args)` — a safe call: evaluates to `null`
    /// when the receiver is null, else the member access / call result.
    SafeCall {
        receiver: ExprId,
        name: String,
        args: Option<Vec<ExprId>>,
    },
    /// `throw operand` — raises an exception; an expression of bottom type `Nothing`.
    Throw {
        operand: ExprId,
    },
    /// `return value` / `return@label value` used in expression position (`x ?: return null`). An
    /// expression of bottom type `Nothing` — it transfers control out of the enclosing function.
    Return {
        value: Option<ExprId>,
        label: Option<String>,
    },
    /// `break` / `break@label` used in EXPRESSION position (`val v = m[k] ?: break`). An expression of
    /// bottom type `Nothing` — it transfers control out of the enclosing (labelled) loop. (A statement-
    /// position `break` is `Stmt::Break`.)
    Break {
        label: Option<String>,
    },
    /// `continue` / `continue@label` used in EXPRESSION position (`m[k] ?: continue`). Bottom type
    /// `Nothing` — it jumps to the next iteration of the enclosing (labelled) loop.
    Continue {
        label: Option<String>,
    },
    /// A lambda literal `{ param -> body }` / `{ body }` (implicit `it`). krusty only supports it as
    /// the trailing argument of an *inlined* scope function (`let`/`also`); `body` is a `Block`.
    Lambda {
        params: Vec<String>,
        body: ExprId,
    },
    /// `try { body } catch (e: T) { … } … [finally { … }]` — the value is the body's, or a matching
    /// catch's; `finally` runs on every exit (for effect). Each `body`/handler/finally is a `Block`.
    Try {
        body: ExprId,
        catches: Vec<CatchClause>,
        finally: Option<ExprId>,
    },
    /// `operand is T` / `operand !is T` — a type test (`instanceof`), evaluates to `Boolean`.
    Is {
        operand: ExprId,
        ty: TypeRef,
        negated: bool,
    },
    /// `operand as T` / `operand as? T` — a cast (`checkcast`). `nullable` ⇒ `as?` (instanceof,
    /// `null` on mismatch). Result type is `T`.
    As {
        operand: ExprId,
        ty: TypeRef,
        nullable: bool,
    },
    /// `value in start..end` / `value !in start..end` — range membership, evaluates to `Boolean`.
    /// `kind` is the range form (`..`/`until`/`downTo`); `negated` ⇒ `!in`. (Range membership only;
    /// a non-range container would resolve `contains`, not yet modeled.)
    InRange {
        value: ExprId,
        start: ExprId,
        end: ExprId,
        kind: RangeKind,
        negated: bool,
    },
    /// `lo..hi` / `lo..<hi` / `lo until hi` / `lo downTo hi` as a *value* — constructs a range
    /// (`IntRange`/`LongRange`) or progression (`IntProgression` for `downTo`). Distinct from the
    /// `for`/`in` forms, which lower to counted loops / membership without materializing the object.
    RangeTo {
        lo: ExprId,
        hi: ExprId,
        kind: RangeKind,
    },
    /// `target++` / `target--` / `++target` / `--target` in *expression* (value) position — yields the
    /// old value (postfix) or new value (prefix) while updating the lvalue. Statement position keeps
    /// `Stmt::IncDec` / the member-index desugar (value discarded). `target` is currently a `Name`.
    IncDec {
        target: ExprId,
        dec: bool,
        prefix: bool,
    },
    Unary {
        op: UnOp,
        operand: ExprId,
    },
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        /// Source span of the operator token.
        operator_span: Span,
    },
    /// `receiver.name` (no call). For a bare name use `Name`.
    Member {
        receiver: ExprId,
        name: String,
    },
    /// `array[i]` / `receiver[i, j, …]` — a subscript. A SINGLE index is an array element access or a
    /// unary `get` operator; TWO OR MORE is always a `get(i, j, …)` operator (there is no built-in
    /// multi-dimensional array in Kotlin). `indices` always has at least one element.
    Index {
        array: ExprId,
        indices: Vec<ExprId>,
    },
    /// `callee(args)`. `callee` is `Name` (free function) or `Member` (method).
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    If {
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    /// `{ stmts; trailing? }` — block as an expression; trailing expr is its value.
    Block {
        stmts: Vec<StmtId>,
        trailing: Option<ExprId>,
    },
    /// `when (subject?) { conditions -> body ; else -> body }`. An arm with empty `conditions` is
    /// the `else`. With a subject, each condition is a value matched by `==`; without, each is a
    /// boolean expression.
    When {
        subject: Option<ExprId>,
        arms: Vec<WhenArm>,
    },
    /// `receiver::name` or `::name` (top-level) — a callable reference or class literal.
    /// krusty parses these to avoid cascade errors but does not implement them at runtime.
    CallableRef {
        receiver: Option<ExprId>,
        name: String,
    },
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub name: String,
    pub ty: TypeRef,
    pub body: ExprId,
}

#[derive(Clone, Debug)]
pub struct WhenArm {
    /// Empty ⇒ the `else` arm.
    pub conditions: Vec<ExprId>,
    pub body: ExprId,
}

#[derive(Clone, Debug)]
pub enum TemplatePart {
    Str(KtString),
    Expr(ExprId),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// `val`/`var name (: type)? = init`
    Local {
        is_var: bool,
        name: String,
        ty: Option<TypeRef>,
        init: ExprId,
    },
    /// `lateinit var name: type` — a mutable local with no initializer (the slot defaults to `null`); a
    /// read while still null throws `UninitializedPropertyAccessException`. Kept distinct from `Local`
    /// (whose initializer is mandatory).
    LocalLateinit {
        name: String,
        ty: TypeRef,
    },
    /// `val`/`var name (: type)? by delegate` — a local delegated property. Reads route through the
    /// delegate's `getValue`; a `var`'s writes through `setValue`. No backing local of its own (only the
    /// synthesized `$delegate` local holds the delegate instance).
    LocalDelegate {
        is_var: bool,
        name: String,
        ty: Option<TypeRef>,
        delegate: ExprId,
    },
    /// `val (a, b, …) = init` — destructuring; each entry binds `init.componentN()`.
    /// An entry named `_` is skipped (no binding, no `componentN` call), per Kotlin.
    Destructure {
        entries: Vec<(String, bool)>,
        init: ExprId,
    },
    /// `name = value`
    Assign {
        name: String,
        value: ExprId,
    },
    /// `name++` / `name--` / `++name` / `--name` in statement position — the increment/decrement
    /// operator on a simple variable. Kept as a real node (not desugared) because `inc`/`dec` are
    /// overloadable operators; the checker resolves built-in numeric inc/dec vs a user operator.
    IncDec {
        name: String,
        dec: bool,
        /// `++name` (true) vs `name++` (false). Semantically irrelevant in statement position, but
        /// kotlinc's bytecode shape differs (a postfix spills the old value to a temp; a prefix
        /// re-reads and pops), so it's kept for byte parity.
        prefix: bool,
    },
    /// `receiver.name = value` — write a (mutable) property via its setter.
    AssignMember {
        receiver: ExprId,
        name: String,
        value: ExprId,
    },
    /// `array[i] = value` / `receiver[i, j, …] = value` — a subscript store (an array element store for
    /// a single index, else the `set(i, j, …, value)` operator). `indices` always has at least one.
    AssignIndex {
        array: ExprId,
        indices: Vec<ExprId>,
        value: ExprId,
    },
    /// `return [expr]` (no label → returns from the enclosing function) or `return@label [expr]`
    /// (`Some(label)` → a *local* return from the lambda carrying that label — the common
    /// `forEach { return@forEach }` form; for an inline-spliced lambda the label is the inline fn name).
    Return(Option<ExprId>, Option<String>),
    /// `break` / `continue` — loop control. `Some(label)` targets the enclosing loop carrying that
    /// `label@` (`break@outer`); `None` targets the innermost loop.
    Break(Option<String>),
    Continue(Option<String>),
    While {
        cond: ExprId,
        body: ExprId,
        label: Option<String>,
    }, // body is a Block expr
    /// `do { body } while (cond)` — post-test loop (body runs at least once).
    DoWhile {
        body: ExprId,
        cond: ExprId,
        label: Option<String>,
    },
    /// `for (name in start <op> end (step s)?) body` over an integer range.
    For {
        name: String,
        range: ForRange,
        body: ExprId,
        label: Option<String>,
    },
    /// `for (name in iterable) body` over an array (element iteration).
    ForEach {
        name: String,
        iterable: ExprId,
        body: ExprId,
        label: Option<String>,
    },
    Expr(ExprId),
    /// A local function declaration: `fun name(params): Ret { body }` inside a function body.
    /// Emitted as a private static method on the file/class with a mangled name.
    LocalFun(FunDecl),
    /// A local class/object/interface declared inside a function body. Hoisted (signature collection
    /// walks fn bodies) to a top-level-equivalent class with a mangled internal name, so the checker
    /// and lowering treat it like any other class. A capturing local class fails to resolve its outer
    /// references (it's checked with no enclosing scope) → the file skips, never miscompiles.
    LocalClass(ClassDecl),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeKind {
    Through, // a..b   (inclusive)
    Until,   // a until b (exclusive)
    DownTo,  // a downTo b (descending, inclusive)
}

#[derive(Clone, Debug)]
pub struct ForRange {
    pub start: ExprId,
    pub end: ExprId,
    pub kind: RangeKind,
}

/// Bit-packed [`TypeRef`] flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrFlags(u8);

impl TrFlags {
    const NULLABLE: u8 = 1 << 0;
    const DEFINITELY_NON_NULL: u8 = 1 << 1;
    const FUN_HAS_RECEIVER: u8 = 1 << 2;
    const FUN_SUSPEND: u8 = 1 << 3;
    const IN_PROJECTION: u8 = 1 << 4;
    const OUT_PROJECTION: u8 = 1 << 5;
    const IMPORT: u8 = 1 << 6;
    // Parsing still represents `*` as its semantic upper bound (`Any?`) for ordinary type
    // resolution, but an `is FunctionN<*, ...>` check must distinguish that runtime-checkable
    // projection from an explicitly written `Any?`. Preserve the source distinction in the last
    // available flag bit instead of recovering it from source text in individual consumers.
    const STAR_PROJECTION: u8 = 1 << 7;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_nullable(self, on: bool) -> Self {
        self.with(Self::NULLABLE, on)
    }
    #[inline]
    pub const fn with_definitely_non_null(self, on: bool) -> Self {
        self.with(Self::DEFINITELY_NON_NULL, on)
    }
    #[inline]
    pub const fn with_fun_has_receiver(self, on: bool) -> Self {
        self.with(Self::FUN_HAS_RECEIVER, on)
    }
    #[inline]
    pub const fn with_fun_suspend(self, on: bool) -> Self {
        self.with(Self::FUN_SUSPEND, on)
    }
    #[inline]
    pub const fn with_in_projection(self, on: bool) -> Self {
        self.with(Self::IN_PROJECTION, on)
    }
    #[inline]
    pub const fn with_out_projection(self, on: bool) -> Self {
        self.with(Self::OUT_PROJECTION, on)
    }
    #[inline]
    pub const fn with_import(self, on: bool) -> Self {
        self.with(Self::IMPORT, on)
    }
    #[inline]
    pub const fn with_star_projection(self, on: bool) -> Self {
        self.with(Self::STAR_PROJECTION, on)
    }
}

#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: String,
    /// Nullability, function-type shape, and use-site projection flags.
    pub flags: TrFlags,
    /// The first generic type argument, captured for `Array<T>` (element) and function types
    /// (the return type). General class type arguments live in `targs`.
    pub arg: Option<Box<TypeRef>>,
    /// All generic type arguments on a class type (`Map<K, V>` → `[K, V]`). Empty for non-generic
    /// types. JVM-erased in descriptors but kept so the front end recovers member/element types.
    pub targs: Vec<TypeRef>,
    pub span: Span,
    /// For function types `(A, B) -> R`: the parameter types. Empty for non-function types.
    /// When non-empty, `name` is `"<fun>"` and `arg` holds the return type.
    pub fun_params: Vec<TypeRef>,
    /// Leading physical parameters that bind as context receivers in a lambda.
    pub fun_context_count: u32,
}

impl TypeRef {
    #[inline]
    pub fn nullable(&self) -> bool {
        self.flags.has(TrFlags::NULLABLE)
    }
    #[inline]
    pub fn definitely_non_null(&self) -> bool {
        self.flags.has(TrFlags::DEFINITELY_NON_NULL)
    }
    #[inline]
    pub fn fun_has_receiver(&self) -> bool {
        self.flags.has(TrFlags::FUN_HAS_RECEIVER)
    }
    #[inline]
    pub fn fun_suspend(&self) -> bool {
        self.flags.has(TrFlags::FUN_SUSPEND)
    }
    #[inline]
    pub fn in_projection(&self) -> bool {
        self.flags.has(TrFlags::IN_PROJECTION)
    }
    #[inline]
    pub fn out_projection(&self) -> bool {
        self.flags.has(TrFlags::OUT_PROJECTION)
    }
    #[inline]
    pub fn is_import(&self) -> bool {
        self.flags.has(TrFlags::IMPORT)
    }
    #[inline]
    pub fn is_star_projection(&self) -> bool {
        self.flags.has(TrFlags::STAR_PROJECTION)
    }
    #[inline]
    pub fn set_nullable(&mut self, on: bool) {
        self.flags = self.flags.with_nullable(on);
    }
    #[inline]
    pub fn set_definitely_non_null(&mut self, on: bool) {
        self.flags = self.flags.with_definitely_non_null(on);
    }
    #[inline]
    pub fn set_projection(&mut self, in_projection: bool, out_projection: bool) {
        self.flags = self
            .flags
            .with_in_projection(in_projection)
            .with_out_projection(out_projection);
    }
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: TypeRef,
    /// `true` for a `vararg` parameter — its runtime type is `Array<ty>` and callers pack the
    /// trailing arguments into a fresh array.
    pub is_vararg: bool,
    /// Default value (`fun f(x: Int = 5)`). Filled in at the call site for omitted trailing
    /// arguments. Defaults that reference another parameter are rejected (see resolve.rs).
    pub default: Option<ExprId>,
    /// Simple names of annotations applied to the parameter (`@IntroducedAt("1") b: String` →
    /// `["IntroducedAt"]`). Used by the compiler-extension surface.
    pub annotations: Vec<String>,
    /// The argument expressions of each annotation in `annotations` (same order/length): an extension
    /// that needs an annotation's value (`@SerialName("foo")`) reads `annotation_args[i][0]`. An empty
    /// inner vec for a no-arg annotation.
    pub annotation_args: Vec<Vec<ExprId>>,
}

#[derive(Clone, Debug)]
pub enum FunBody {
    Expr(ExprId),
    Block(ExprId), // a Block expr
    None,          // (no body — not valid for v0 top-level, but parseable)
}

/// Bit-packed boolean modifiers for a [`FunDecl`], collapsing its eight `is_*` modifier bytes into one
/// `u8` (a real 8-byte-per-decl saving). Read through the `FunDecl` accessors of the same names;
/// `is_open`/`is_override`/`is_operator` are mutated through the matching `set_*` methods; built with
/// the `with_*` chain. All eight bits are in use — a ninth flag needs a wider field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FdFlags(u8);

impl FdFlags {
    const IS_INLINE: u8 = 1 << 0;
    const IS_FINAL: u8 = 1 << 1;
    const IS_OPEN: u8 = 1 << 2;
    const IS_OVERRIDE: u8 = 1 << 3;
    const IS_ABSTRACT: u8 = 1 << 4;
    const IS_SUSPEND: u8 = 1 << 5;
    const IS_TAILREC: u8 = 1 << 6;
    const IS_OPERATOR: u8 = 1 << 7;

    #[inline]
    const fn with(mut self, mask: u8, on: bool) -> Self {
        if on {
            self.0 |= mask;
        } else {
            self.0 &= !mask;
        }
        self
    }
    #[inline]
    const fn has(self, mask: u8) -> bool {
        self.0 & mask != 0
    }

    #[inline]
    pub const fn with_is_inline(self, on: bool) -> Self {
        self.with(Self::IS_INLINE, on)
    }
    #[inline]
    pub const fn with_is_final(self, on: bool) -> Self {
        self.with(Self::IS_FINAL, on)
    }
    #[inline]
    pub const fn with_is_open(self, on: bool) -> Self {
        self.with(Self::IS_OPEN, on)
    }
    #[inline]
    pub const fn with_is_override(self, on: bool) -> Self {
        self.with(Self::IS_OVERRIDE, on)
    }
    #[inline]
    pub const fn with_is_abstract(self, on: bool) -> Self {
        self.with(Self::IS_ABSTRACT, on)
    }
    #[inline]
    pub const fn with_is_suspend(self, on: bool) -> Self {
        self.with(Self::IS_SUSPEND, on)
    }
    #[inline]
    pub const fn with_is_tailrec(self, on: bool) -> Self {
        self.with(Self::IS_TAILREC, on)
    }
    #[inline]
    pub const fn with_is_operator(self, on: bool) -> Self {
        self.with(Self::IS_OPERATOR, on)
    }
}

#[derive(Clone, Debug)]
pub struct FunDecl {
    pub name: String,
    /// Extension receiver type (`fun String.foo()` → `Some("String")`). Emitted as a static
    /// method with the receiver prepended as the first parameter.
    pub receiver: Option<TypeRef>,
    pub params: Vec<Param>,
    /// Number of LEADING entries in `params` that are context parameters (`context(a: A) fun f()`).
    /// kotlinc lowers context receivers to leading value parameters; krusty models them the same way,
    /// so they are ordinary params for the body/signature, and this count lets the call-site resolver
    /// fill them IMPLICITLY from the enclosing context (an implicit receiver / an outer context param)
    /// instead of from positional arguments. `0` for a function without context parameters.
    pub context_count: usize,
    pub ret: Option<TypeRef>,
    pub body: FunBody,
    /// Generic type-parameter names (`fun <T, U> …`), erased to `Any`/`Object`.
    pub type_params: Vec<String>,
    /// Declared non-`Any` upper bounds (`fun <T: Int> …` → `("T", Int)`). A PRIMITIVE bound makes the
    /// parameter specialized to that primitive (kotlinc emits `(I)I`, not `(Object)Object`), like a
    /// value class's underlying type — see `ClassDecl::type_param_bounds`.
    pub type_param_bounds: Vec<(String, TypeRef)>,
    /// Subset of `type_params` that carry an `Any` upper bound (`T: Any`) — non-nullable on JVM.
    pub non_null_type_params: std::collections::HashSet<String>,
    /// Subset of `type_params` declared `reified` (only meaningful on an `inline` function): the body
    /// may use them concretely (`is T`, `as T`, `T::class`) and codegen specializes them per call.
    pub reified_type_params: std::collections::HashSet<String>,
    pub span: Span,
    /// Source range from `fun` through the optional `where` clause, excluding the body.
    pub signature_span: Span,
    /// 1-based source line of the `fun` declaration (from `span.lo`), for its `LineNumberTable`.
    /// 0 = unknown (no debug table emitted). Filled by the same parser post-pass as `Class::decl_line`.
    pub decl_line: u32,
    /// 1-based source line of a BLOCK body's closing `}` — kotlinc maps a `Unit` function's implicit
    /// `return` to this line in the `LineNumberTable`. 0 = unknown / expression body. Filled by the
    /// same parser post-pass as `decl_line`.
    pub body_close_line: u32,
    /// Bit-packed `is_inline`/`is_final`/`is_open`/`is_override`/`is_abstract`/`is_suspend`/`is_tailrec`/
    /// `is_operator` (read via the accessors below; `is_open`/`is_override`/`is_operator` set via `set_*`).
    /// `is_final` — `final`, cannot be overridden. `is_open` — `open`/`override` without `final`, so the
    /// JVM backend must NOT emit `ACC_FINAL`. `is_override` — the member MUST match a supertype member.
    /// `is_abstract` — no body, only valid in an abstract class/interface. `is_suspend` — a coroutine,
    /// lowered CPS with a trailing `Continuation`. `is_tailrec` — a self-recursive tailrec rewritten to a
    /// loop. `is_inline`/`is_operator` — the `inline`/`operator` modifiers.
    pub flags: FdFlags,
    /// Declaration visibility (`public`/`internal`/`protected`/`private`; `public` by default).
    /// Public/internal/protected functions get `Intrinsics.checkNotNullParameter` guards on their
    /// non-null reference parameters (kotlinc does); private ones do not (read via `visibility.is_private()`).
    pub visibility: Visibility,
    /// Simple names of annotations applied to this function (`@Composable fun f()` → `["Composable"]`),
    /// mirroring `ClassDecl.annotations`. Used by the compiler-extension surface (`crate::plugins`) to
    /// find annotated functions.
    pub annotations: Vec<String>,
    /// The argument expressions of each annotation in [`Self::annotations`] (same order/length),
    /// mirroring `ClassDecl::annotation_args`. `@JvmName("gNullable")` reads its bytecode name here.
    pub annotation_args: Vec<Vec<ExprId>>,
}

impl FunDecl {
    /// The bytecode method name this function is emitted under: the `@JvmName("…")` spelling when the
    /// annotation is present with a constant string argument, otherwise the source name.
    ///
    /// The JVM name — not the source name — is the identity that decides a platform declaration
    /// clash, so two overloads erasing to the same descriptor (`g(String)` / `g(String?)`) are legal
    /// exactly when `@JvmName` separates them, as in kotlinc.
    pub fn jvm_name(&self, file: &File) -> String {
        self.annotations
            .iter()
            .position(|a| a.rsplit(['/', '.']).next().unwrap_or(a) == "JvmName")
            .and_then(|i| self.annotation_args.get(i)?.first())
            .and_then(|&arg| match file.expr(arg) {
                // A JVM method NAME: an unpaired surrogate has no name form, so fall back to the
                // declared name rather than corrupt the descriptor.
                Expr::StringLit(s) => s.as_str().map(str::to_string),
                _ => None,
            })
            .unwrap_or_else(|| self.name.clone())
    }

    pub(crate) fn has_callable_inline_extension_body(&self) -> bool {
        // Emit the inline fn as a REAL (static) method too, like kotlinc does — a separate
        // compilation can then resolve and splice it. Type parameters (incl. `reified`) are fine:
        // the emitted body is erased, callers splice with call-site bindings. (A reified fn whose
        // BODY uses the parameter would need kotlinc's reifiedOperationMarker to fault direct
        // calls — not modeled; krusty callers always splice or bail.) Scoped to EXTENSIONS with
        // value-typed parameters: broader emission reshapes cross-module inline resolution in
        // ways the splice machinery doesn't cover (defaults, captures, non-local-return lambdas).
        self.is_inline()
            && self.receiver.is_some()
            && self.params.iter().all(|parameter| {
                parameter.ty.name != "<fun>"
                    && parameter.ty.fun_params.is_empty()
                    && !parameter.ty.fun_suspend()
            })
    }
    #[inline]
    pub fn is_inline(&self) -> bool {
        self.flags.has(FdFlags::IS_INLINE)
    }
    #[inline]
    pub fn is_final(&self) -> bool {
        self.flags.has(FdFlags::IS_FINAL)
    }
    #[inline]
    pub fn is_open(&self) -> bool {
        self.flags.has(FdFlags::IS_OPEN)
    }
    #[inline]
    pub fn is_override(&self) -> bool {
        self.flags.has(FdFlags::IS_OVERRIDE)
    }
    #[inline]
    pub fn is_abstract(&self) -> bool {
        self.flags.has(FdFlags::IS_ABSTRACT)
    }
    #[inline]
    pub fn is_suspend(&self) -> bool {
        self.flags.has(FdFlags::IS_SUSPEND)
    }
    #[inline]
    pub fn is_tailrec(&self) -> bool {
        self.flags.has(FdFlags::IS_TAILREC)
    }
    #[inline]
    pub fn is_operator(&self) -> bool {
        self.flags.has(FdFlags::IS_OPERATOR)
    }
    #[inline]
    pub fn set_is_open(&mut self, on: bool) {
        self.flags = self.flags.with_is_open(on);
    }
    #[inline]
    pub fn set_is_override(&mut self, on: bool) {
        self.flags = self.flags.with_is_override(on);
    }
    #[inline]
    pub fn set_is_operator(&mut self, on: bool) {
        self.flags = self.flags.with_is_operator(on);
    }
}

/// A primary-constructor parameter that is also a property (`val`/`var name: Type`).
/// v0: property types are restricted to the primitive/String `Ty` set (no class-typed members yet).
#[derive(Clone, Debug)]
pub struct PropParam {
    pub name: String,
    pub ty: TypeRef,
    /// `true` for a `vararg` primary-constructor parameter — its declared element type `ty` is exposed
    /// as `Array<ty>` (a backing field/property of the array type); callers pack trailing arguments.
    pub is_vararg: bool,
    pub is_var: bool,
    /// `true` for a `val`/`var` parameter (a property → backing field + accessor); `false` for a
    /// plain constructor parameter (in scope for `init`/body-property initializers, but not a field).
    pub is_property: bool,
    pub is_override: bool,
    /// `open` or `override` without `final`.
    pub is_open: bool,
    /// Declaration visibility (`public` by default), from the constructor-parameter modifier list.
    /// A `private` property's backing field gets NO accessor (kotlinc reads it directly in-class), so
    /// the accessor synthesis skips it; `internal`/`protected` currently accessor like `public`.
    pub visibility: Visibility,
    /// Default value (`class C(val x: Int = 5)`). Used to synthesize a no-arg constructor when
    /// all primary-constructor parameters have defaults.
    pub default: Option<ExprId>,
    /// Simple names of annotations on this constructor parameter (`@SerialName("x") val a` →
    /// `["SerialName"]`); empty for none. Read by the compiler-extension surface.
    pub annotations: Vec<String>,
    /// The argument expressions of each annotation in `annotations` (same order/length) — kept so an
    /// extension can const-fold a value (`@SerialName("$prefix.bar")`). Empty inner vec for a no-arg
    /// annotation.
    pub annotation_args: Vec<Vec<ExprId>>,
    /// Source span of the parameter name (zero when synthesized, e.g. an inner class's captured
    /// outer). Filled by the parser; the post-pass derives `decl_line` from it.
    pub span: Span,
    /// 1-based source line of this parameter (0 = unknown). kotlinc's primary-constructor
    /// `LineNumberTable` maps each property parameter's field store to the parameter's own line.
    pub decl_line: u32,
}

/// One entry of an `enum class` (`RED(0xFF0000) { override fun m() = … }`). Groups what were parallel
/// `Vec`s keyed by entry index (name / constructor args / per-entry-body methods / per-entry-body
/// properties), so an entry's four facets can't desync.
#[derive(Clone, Debug)]
pub struct AstEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Source span of the entry name.
    pub span: Span,
    /// 1-based source line of the entry, filled by the parser post-pass (0 = unknown). kotlinc gives
    /// each entry's construction in `<clinit>` its own `LineNumberTable` entry.
    pub decl_line: u32,
    /// Simple names of annotations on this constant (`@SerialName("x") RED` → `["SerialName"]`),
    /// parallel to `annotation_args`. Emitted onto the enum's static field (per JVM retention).
    pub annotations: Vec<String>,
    /// The argument expressions of each annotation in `annotations` (same order/length).
    pub annotation_args: Vec<Vec<ExprId>>,
    /// Constructor arguments (`RED(0xFF0000)` → the two arg expr ids); empty for `RED` with no args.
    pub args: Vec<ExprId>,
    /// Per-argument name for a NAMED argument (`RED(rgb = 0xFF0000)`), parallel to `args`; `None` for
    /// a positional argument. Lets the lowering reorder named/omitted arguments to constructor order.
    pub arg_names: Vec<Option<String>>,
    /// Per-entry class-body method overrides (`RED { override fun m() = … }`) — the anonymous subclass
    /// kotlinc emits as `Enum$RED`. Empty when the entry has no body.
    pub methods: Vec<FunDecl>,
    /// Per-entry class-body properties (`RED { val y = … }`) — backing fields + getters on the
    /// `Enum$RED` subclass. Empty when the entry has none.
    pub props: Vec<PropDecl>,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub name: String,
    /// Declaration visibility (`public` by default).
    pub visibility: Visibility,
    /// Simple names of annotations applied to the class (`@Serializable` → `["Serializable"]`).
    /// Used by the compiler-extension surface (`crate::plugins`) to find annotated declarations.
    pub annotations: Vec<String>,
    /// The argument expressions of each annotation in `annotations` (same order/length) — kept so an
    /// extension can read an annotation's value (`@Serializable(with = X::class)`). Empty inner vec for
    /// a no-arg annotation.
    pub annotation_args: Vec<Vec<ExprId>>,
    /// Generic type-parameter names (`class C<T>`), erased to `Any`/`Object`.
    pub type_params: Vec<String>,
    /// Declared non-`Any` upper bounds (`<T: String>` → `("T", String)`). A value class's underlying
    /// type parameter erases to its bound (`value class S<T: String>(val x: T)` → `String`), like kotlinc.
    pub type_param_bounds: Vec<(String, TypeRef)>,
    pub props: Vec<PropParam>,
    /// Member functions declared in the class body (instance methods). v0: no secondary ctors.
    pub methods: Vec<FunDecl>,
    /// `companion object { … }` member functions — emitted as `static` methods on this class and
    /// called as `ClassName.fn(...)`.
    pub companion_methods: Vec<FunDecl>,
    /// `companion object { … }` properties (`const val`/`val`) — emitted as `static final` fields and
    /// read as `ClassName.PROP`.
    pub companion_props: Vec<PropDecl>,
    /// A `companion object`'s declared base CLASS (`companion object : Base(args)`), if any — the
    /// synthesized `C$Companion` extends it (instead of `kotlin/Any`) and its ctor calls `super(args)`.
    pub companion_base: Option<String>,
    /// The `super(args)` arguments for [`companion_base`].
    pub companion_base_args: Vec<ExprId>,
    /// A `companion object`'s declared interface supertypes (`companion object : I1, I2`).
    pub companion_supertypes: Vec<String>,
    /// Source line of the `companion object` declaration (0 when absent).
    pub companion_decl_line: u32,
    /// Properties declared in the class *body* (`class C { val x = … }`) — backing field + accessor,
    /// initialized in the primary constructor.
    pub body_props: Vec<PropDecl>,
    /// Constructor init steps in source order: a body-property initializer (index into `body_props`)
    /// or an `init { … }` block.
    pub init_order: Vec<ClassInit>,
    /// The declaration kind (plain class / interface / object / enum / annotation). One field instead
    /// of parallel `is_*` booleans; read it through the `is_*` accessor methods.
    pub kind: ClassKind,
    /// `data class` — synthesizes equals/hashCode/toString/componentN/copy.
    pub is_data: bool,
    /// `@JvmInline value class` — an inline class. krusty currently compiles it as a regular final
    /// single-field class (self-consistent, box-OK) rather than kotlinc's unboxed `-impl` form.
    pub is_value: bool,
    /// `enum class Name { A, B }` — the entries in declaration order (extends `java/lang/Enum`). Each
    /// [`AstEnumEntry`] carries its own name / constructor args / body methods / body properties.
    pub enum_entries: Vec<AstEnumEntry>,
    /// `fun interface Name { fun m(…): R }` — a SAM (single-abstract-method) interface; a lambda is
    /// convertible to it.
    pub is_fun_interface: bool,
    /// Inheritance modality (`final` / `open` / `abstract` / `sealed`). Replaces the old
    /// `is_open` + `is_abstract` + `is_sealed` booleans; read via the `is_open()` / `is_abstract()` /
    /// `is_sealed()` accessors (which preserve the prior bool semantics, incl. `sealed ⟹ abstract+open`).
    pub modality: Modality,
    /// `inner class` — captures the enclosing instance: emitted with a synthetic `this$0` field of the
    /// outer type (the first field + first constructor parameter). `Some(outer_class_simple_name)`.
    pub inner_of: Option<String>,
    /// Implemented interface names from a supertype list (`class C : I1, I2`).
    /// Implemented interfaces (NOT the base class — that's `base_class`), each as a full `TypeRef` so its
    /// type arguments are preserved (`Operation<Result<Int>>`), for the class `Signature` attribute and
    /// any downstream generic-supertype reasoning. Read `.name` for the bare simple name.
    pub supertypes: Vec<TypeRef>,
    /// Interface delegation `: Iface by delegate` — `(iface simple name, delegate variable name,
    /// has_primitive_targ)`. The class forwards each of `Iface`'s methods to `delegate` (a `val`
    /// constructor-parameter field). `has_primitive_targ` is true when the delegated interface is
    /// instantiated with a non-nullable primitive type argument (`A<Long>`): such a forwarder needs
    /// substituted-type bridges a raw (erased-`Object`) forward mis-coerces, so it is skipped.
    pub delegations: Vec<(String, String, bool)>,
    /// Interface delegation to an EXPRESSION `: Iface by <expr>` (`by Impl()`) — `(iface simple name,
    /// delegate expression)`. The expression is evaluated once into a synthesized `$$delegate_e<j>`
    /// field (stored in the constructor); each of `Iface`'s methods forwards to that field.
    pub delegation_exprs: Vec<(String, ExprId)>,
    /// A base-class supertype `: Base(args)` (name + constructor arguments), if any.
    pub base_class: Option<String>,
    pub base_type_args: Vec<TypeRef>,
    pub base_args: Vec<ExprId>,
    /// Secondary constructors: `constructor(params) : this/super(args) { body }`.
    pub secondary_ctors: Vec<SecondaryCtor>,
    /// `false` when the class declares NO primary constructor (`class A { constructor(...) }`): every
    /// constructor is a secondary, and a `super(...)`/implicit-delegating one (not `this(...)`) runs the
    /// field initializers + `init {}` blocks. `true` for an implicit/explicit primary (`class A`,
    /// `class A()`, `class A(...)`), including a `class A() { constructor(...) : this(...) }`.
    pub has_primary_ctor: bool,
    pub span: Span,
    /// 1-based source line of the class declaration (from `span.lo`), for the `LineNumberTable` of
    /// kotlinc's synthesized members (ctor/accessors), which all map to the class's declaration line.
    /// 0 = unknown (no debug tables emitted). Filled by a parser post-pass.
    pub decl_line: u32,
}

/// What a declaration *is*. Mutually exclusive at the source level (`data`/`value` are modifiers on a
/// `Class`, `fun interface` is `Interface` + `is_fun_interface`). An `annotation class` compiles to a
/// JVM interface, but the front end keeps it distinct from `Interface` — `is_interface()` is `false`
/// for it (matching the parser, which never set `is_interface` on annotations).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClassKind {
    Class,
    Interface,
    /// `object Name { … }` — a singleton (one `INSTANCE`, private constructor).
    Object,
    /// `enum class Name { A, B }` — extends `java/lang/Enum`.
    Enum,
    /// `annotation class` — emitted as an interface extending `java/lang/annotation/Annotation`;
    /// instantiation (`A("x")`) synthesizes a `<facade>$annotationImpl$A$0` impl class.
    Annotation,
}

/// A class's inheritance modality. One field instead of parallel `is_open`/`is_abstract`/`is_sealed`
/// booleans (which encoded `sealed ⟹ abstract` and `sealed ⟹ open` only by convention). Read through
/// the accessor methods, which reproduce the old boolean values exactly.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Modality {
    /// `final` (the Kotlin default) — cannot be subclassed.
    #[default]
    Final,
    /// `open` — may be subclassed, but is not `abstract`.
    Open,
    /// `abstract` — not `final`; carries `ACC_ABSTRACT`.
    Abstract,
    /// `sealed` — abstract, open, and its subclasses are all known in this module.
    Sealed,
}

impl Modality {
    /// `abstract` OR `sealed` — both carry `ACC_ABSTRACT` (matches the old `is_abstract` bool).
    pub fn is_abstract(self) -> bool {
        matches!(self, Modality::Abstract | Modality::Sealed)
    }
    /// `open` OR `sealed` — subclassable without `abstract` (matches the old `is_open` bool, which the
    /// parser set as `sealed || open` and NOT for a bare `abstract`).
    pub fn is_open(self) -> bool {
        matches!(self, Modality::Open | Modality::Sealed)
    }
    /// Specifically `sealed`.
    pub fn is_sealed(self) -> bool {
        matches!(self, Modality::Sealed)
    }
    pub fn is_final(self) -> bool {
        matches!(self, Modality::Final)
    }
}

impl ClassDecl {
    /// `abstract` or `sealed` (both carry `ACC_ABSTRACT`).
    pub fn is_abstract(&self) -> bool {
        self.modality.is_abstract()
    }
    /// `open` or `sealed` (subclassable without `abstract`).
    pub fn is_open(&self) -> bool {
        self.modality.is_open()
    }
    /// Specifically `sealed`.
    pub fn is_sealed(&self) -> bool {
        self.modality.is_sealed()
    }
    pub fn is_final(&self) -> bool {
        self.modality.is_final() && !self.is_interface()
    }
    pub fn is_interface(&self) -> bool {
        self.kind == ClassKind::Interface
    }
    pub fn is_object(&self) -> bool {
        self.kind == ClassKind::Object
    }
    pub fn is_enum(&self) -> bool {
        self.kind == ClassKind::Enum
    }
    pub fn is_annotation(&self) -> bool {
        self.kind == ClassKind::Annotation
    }
}

/// A secondary constructor `constructor(params) [: this(args) | : super(args)] [{ body }]`.
#[derive(Clone, Debug)]
pub struct SecondaryCtor {
    pub params: Vec<Param>,
    pub delegation: CtorDelegation,
    pub body: Option<ExprId>,
    pub span: Span,
}

/// How a secondary constructor delegates: to another constructor of the same class (`this(...)`),
/// to a base-class constructor (`super(...)`), or implicitly (none written).
#[derive(Clone, Debug)]
pub enum CtorDelegation {
    None,
    This(CtorDelegationCall),
    Super(CtorDelegationCall),
}

#[derive(Clone, Debug)]
pub struct CtorDelegationCall {
    pub args: Vec<ExprId>,
    pub names: Vec<Option<String>>,
    /// Whether the last argument was written as a SYNTACTIC trailing lambda (`f(1) {}`). A `this(…)` /
    /// `super(…)` delegation can never have one; a constructor CALL can, and the distinction decides
    /// whether that argument may fill a `vararg` slot.
    pub trailing_lambda: bool,
}

/// A class with NO primary constructor names its base class WITHOUT parentheses — `class D : Base {
/// constructor(): super(…) }` — because the base arguments come from each secondary `super(…)`. The
/// parser parks every parenless supertype in `supertypes` and promotes only the ones naming a class
/// declared in the SAME FILE (`fixup_parenless_base_classes`); it cannot see another source file or
/// a classpath, where a base class and an interface look identical. Semantic callers pass one
/// origin-neutral `is_base_class` query and get back the supertype that is really the base.
pub fn parenless_base_supertype(
    class: &ClassDecl,
    mut is_base_class: impl FnMut(&str) -> bool,
) -> Option<&str> {
    if class.has_primary_ctor || class.base_class.is_some() {
        return None;
    }
    let delegates_to_super = class.secondary_ctors.iter().any(|constructor| {
        matches!(
            constructor.delegation,
            CtorDelegation::Super(_) | CtorDelegation::None
        )
    });
    if !delegates_to_super {
        return None;
    }
    class
        .supertypes
        .iter()
        .map(|supertype| supertype.name.as_str())
        .find(|name| is_base_class(name))
}

/// A primary-constructor init step (source-ordered): a body-property initializer or an `init` block.
#[derive(Clone, Debug)]
pub enum ClassInit {
    PropInit(usize), // index into ClassDecl.body_props
    Block(ExprId),   // an `init { … }` block expression
}

#[derive(Clone, Debug)]
pub struct ExplicitBackingField {
    pub ty: Option<TypeRef>,
}

/// A top-level `val`/`var` property: `val name: Type = init`.
#[derive(Clone, Debug)]
pub struct PropDecl {
    pub name: String,
    /// Parameters from a preceding `context(...)` clause.
    pub context_params: Vec<Param>,
    /// 1-based source line of the declaration, filled by the parser post-pass (0 = unknown).
    pub decl_line: u32,
    /// Declaration visibility (`public` by default). A `private set` narrows only the SETTER — that
    /// lives on [`PropAccessor::is_private`]; this is the property's (getter's) visibility.
    pub visibility: Visibility,
    /// Generic type parameters declared on an EXTENSION property (`val <T> Array<T>.length: Int`),
    /// scoped over the receiver, declared type, and accessor bodies. Erased to `Any` like a function's.
    pub type_params: Vec<String>,
    /// Declared non-`Any` upper bounds for [`type_params`] (`<T: Number>`), parallel to a function's.
    pub type_param_bounds: Vec<(String, TypeRef)>,
    /// Extension-property receiver type (`val String.foo: T` → `Some("String")`). The getter/setter
    /// are emitted as static `getFoo(Recv)`/`setFoo(Recv, T)` methods, like an extension function.
    pub receiver: Option<TypeRef>,
    pub ty: Option<TypeRef>,
    pub is_var: bool,
    /// `open` or `override` (without `final`) — the accessors are overridable, so the JVM backend
    /// must not emit `ACC_FINAL` on them (same rule as `FunDecl::is_open`).
    pub is_open: bool,
    pub is_override: bool,
    /// `None` for a `lateinit var` (declared without an initializer; the backing field defaults to
    /// null and is assigned later).
    pub init: Option<ExprId>,
    /// `true` if declared `lateinit` — a no-initializer property is only allowed when lateinit
    /// (otherwise it's an abstract/interface property, which krusty rejects).
    pub is_lateinit: bool,
    /// A custom getter body (`val x: T get() = expr`/`get() { … }`). With no initializer and no
    /// `field` reference it is a computed property (no backing field); with an initializer or a
    /// `field` reference it reads the backing field.
    pub getter: Option<FunBody>,
    /// `true` when the custom getter body references `field` — the property then has a real backing
    /// field even without an initializer (assignable once in a constructor), per Kotlin semantics.
    pub getter_reads_field: bool,
    /// A custom setter (`var x … set(v) { field = … }`) or a visibility-only setter (`private set`).
    pub setter: Option<PropAccessor>,
    /// `true` if declared `const val` — a compile-time constant. kotlinc inlines its value at use
    /// sites; krusty doesn't model that, so a const read across declaration order (a member reading a
    /// later const) would observe the uninitialized field. Used to bail such cases.
    pub is_const: bool,
    /// `true` if declared `abstract` — no backing field; emitted as an abstract `getX()` accessor that
    /// a subclass overrides.
    pub is_abstract: bool,
    /// `val x: T by <expr>` — a DELEGATED property. The expression is the delegate; reads route through
    /// `delegate.getValue(thisRef, property)` (and writes through `setValue`). `None` for a plain property.
    pub delegate: Option<ExprId>,
    pub explicit_backing_field: Option<ExplicitBackingField>,
    pub span: Span,
}

/// A property setter (or, in future, a non-default getter): its parameter name, optional body
/// (`None` = default accessor, e.g. `private set`), and whether it is `private`.
#[derive(Clone, Debug)]
pub struct PropAccessor {
    /// Setter parameter name (`set(value) { … }` → `"value"`); `None` for a default-bodied setter.
    pub param: Option<String>,
    /// `None` = default accessor body (just a visibility change); `Some` = explicit body.
    pub body: Option<FunBody>,
    pub is_private: bool,
}

#[derive(Clone, Debug)]
pub enum Decl {
    Fun(FunDecl),
    Class(ClassDecl),
    Property(PropDecl),
}

/// One parsed source file: its package, and arenas for every node kind.
#[derive(Default)]
pub struct File {
    pub package: Option<String>,
    pub is_script: bool,
    /// Fully-qualified import names (e.g. `util.Calc`), used to resolve Java/JDK references.
    pub imports: Vec<String>,
    /// Aliased imports as `(source alias, fully-qualified target)`.
    pub import_aliases: Vec<(String, String)>,
    /// Classifier references that are not retained by another AST node. Import entries are candidates
    /// until semantic resolution confirms that the imported declaration is a type.
    pub detached_type_refs: Vec<TypeRef>,
    /// Number of source lines, including a final empty line after a trailing newline.
    pub source_line_count: u32,
    pub decls: Vec<DeclId>,
    /// Kotlin script statements in source order.
    pub script_body: Option<ExprId>,
    /// Top-level declarations carrying the `expect` modifier (multiplatform headers). A matched
    /// `actual` in the same compiled source set replaces them (see `strip_matched_expects`); an
    /// unmatched `expect` stays and fails checking like any body-less declaration.
    pub expect_decls: Vec<DeclId>,
    pub decl_arena: Vec<Decl>,
    pub expr_arena: Vec<Expr>,
    pub stmt_arena: Vec<Stmt>,
    pub expr_spans: Vec<Span>,
    pub(crate) retained_expr_spans: std::collections::HashMap<ExprId, Span>,
    pub(crate) local_declarations: std::collections::HashSet<DeclId>,
    pub stmt_spans: Vec<Span>,
    /// Sparse source locations for the value-introducing `=` associated with an expression: property
    /// and local initializers, parameter defaults, and assignment RHS expressions. Diagnostics point
    /// at Kotlin's operator location without retaining source text or adding a span field to every AST
    /// node. Keyed by the value expression's `ExprId`; absent for expression bodies and synthetic values.
    pub value_operator_spans: std::collections::HashMap<u32, Span>,
    /// Assignment lvalue spans keyed by statement ID.
    pub assignment_target_spans: std::collections::HashMap<u32, Span>,
    /// 1-based source line of each expression's start (parallel to `expr_spans`; 0 = unknown).
    /// Filled by the parser post-pass for the `LineNumberTable`.
    pub expr_lines: Vec<u32>,
    /// 1-based source line of each expression's syntactic anchor. Member access and calls use the
    /// selector name; other expressions use their start. Parallel to `expr_spans`.
    pub expr_source_lines: Vec<u32>,
    /// 1-based source line containing the end of each expression. Parallel to `expr_spans`.
    pub expr_end_lines: Vec<u32>,
    /// 1-based source line of each statement's start (parallel to `stmt_spans`; 0 = unknown).
    pub stmt_lines: Vec<u32>,
    /// Per-`Expr::Call` argument names: keyed by the call's `ExprId`, parallel to its `args`
    /// (`None` = positional, `Some(name)` = `name = expr`). Absent ⇒ all positional.
    pub call_arg_names: std::collections::HashMap<u32, Vec<Option<String>>>,
    /// Source span of each named argument's label, parallel to `call_arg_names`. Stored only for calls
    /// that have named arguments, so diagnostics can match Kotlin's label location without retaining
    /// source text or adding metadata to positional calls.
    pub call_arg_name_spans: std::collections::HashMap<u32, Vec<Option<Span>>>,
    /// Exact `(` locations for syntactically empty calls. For non-empty calls, diagnostics can point at
    /// the first argument; keeping this map sparse avoids adding location fields to every call node.
    pub empty_call_open_paren_spans: std::collections::HashMap<u32, Span>,
    /// Exact raw name locations when a member name cannot be recovered from the expression end:
    /// backticked members and safe calls with argument lists. Kept sparse rather than widening every
    /// expression node.
    pub exact_member_name_spans: std::collections::HashMap<u32, Span>,
    /// Dot spans for member expressions with trivia between `.` and the member name.
    pub non_adjacent_member_dot_spans: Vec<(u32, Span)>,
    /// `ExprId`s of `Expr::Call`s whose LAST argument is a SYNTACTIC trailing lambda (`f(a) { … }` /
    /// `f { … }`). A trailing lambda always binds to the callee's LAST parameter — preceding parameters
    /// without a positional argument take their defaults — so default-omission lowering must place it in
    /// the last slot, not the next free positional one (`f("x") { }` on `f(a, m = d, builder)` ⇒ `m`
    /// defaults, the lambda fills `builder`).
    pub call_has_trailing_lambda: std::collections::HashSet<u32>,
    /// End offset of the parenthesized portion of calls with trailing lambdas.
    pub trailing_call_close_paren_ends: std::collections::HashMap<u32, u32>,
    /// `ExprId`s of `Expr::Call`s produced from infix-call syntax (`a foo b`). The callee is still the
    /// ordinary `Member { receiver: a, name: "foo" }`, but resolver/lowering need the source form for
    /// primitive builtin names where Kotlin treats `a rem b` differently from `a.rem(b)`.
    pub infix_calls: std::collections::HashSet<u32>,
    /// Explicit type arguments on a call (`Foo<Int>()`, `listOf<String>(…)`), keyed by the call's
    /// `ExprId`. Lets a constructor call carry its instantiation (`ArrayList<Int>()` → `ArrayList<Int>`)
    /// so member/element types resolve. Absent ⇒ no explicit type arguments.
    pub call_type_args: std::collections::HashMap<u32, Vec<TypeRef>>,
    pub anonymous_object_classes: std::collections::HashMap<ExprId, DeclId>,
    /// The hoisted `Decl::Class` of each statement-position local class (`Stmt::LocalClass`). The
    /// declaration carries the class for signature collection and lowering; the STATEMENT is where
    /// the checker enters it, so that it is checked in the lexical scope it was written in rather
    /// than at file level.
    pub local_class_decls: std::collections::HashMap<StmtId, DeclId>,
    /// Explicit parameter type annotations on a lambda literal (`{ x: Int, y -> … }`), keyed by the
    /// lambda's `ExprId`, parallel to its `params`. `None` for an unannotated parameter. Lets the
    /// checker type a *bare-value* lambda (`val f = { x: Int -> x*2 }`) from its own declared types
    /// when no expected function type drives them.
    pub lambda_param_types: std::collections::HashMap<u32, Vec<Option<TypeRef>>>,
    /// `ExprId.0` of each lambda that originated from an ANONYMOUS FUNCTION expression
    /// (`fun (x: Int): Int = …`). Unlike a plain lambda, a bare `return` inside an anonymous function is
    /// a LOCAL return (from the anonymous function itself), so the lowerer must compile its body's
    /// `return` as the closure method's own return rather than a non-local return of the enclosing fn.
    pub anon_fun_lambdas: std::collections::HashSet<u32>,
    /// `ExprId.0` of each lambda literal carrying the `suspend` modifier (`suspend { … }`). The
    /// checker types it as a `suspend (…) -> …` function type; the lowerer builds a
    /// `SuspendLambda` state machine for it instead of a plain `FunctionN` closure.
    pub suspend_lambdas: std::collections::HashSet<u32>,
    /// The EXPLICIT label a lambda literal was written with (`list.forEach outer@{ … }`), keyed by the
    /// lambda's `ExprId.0`. A labelled lambda REPLACES the implicit label (the callee's name) a
    /// `return@name` inside it targets, so the splicer must register `outer`, not `forEach`. Absent ⇒
    /// the lambda is unlabelled and keeps the implicit callee-name label.
    pub lambda_labels: std::collections::HashMap<u32, String>,
    /// NAME-BASED destructuring: for a `Stmt::Destructure` whose entries bind by property NAME
    /// (`val (number = pCProp, text = pCVarProp) = src`), maps the statement's id to the source
    /// property each entry reads (parallel to `entries`); `None` for a positional (`componentN`) entry.
    /// Absent ⇒ the whole destructuring is positional.
    pub destructure_source_props: std::collections::HashMap<u32, Vec<Option<String>>>,
    /// NAMED super-constructor arguments (`class D : Base(name = …, addr = …)`): the per-argument name
    /// (parallel to the class's `base_args`; `None` for a positional arg), keyed by the FIRST base
    /// argument's `ExprId.0`. The checker/lowerer reorder the base args to the base constructor's
    /// parameter order before use. Absent ⇒ all base args are positional.
    pub base_arg_names: std::collections::HashMap<u32, Vec<Option<String>>>,
    /// Declared return type of an anonymous function (`fun (…): T = …`), keyed by the desugared
    /// lambda's `ExprId.0`. A block body that ends in `return` has body type `Nothing`, so the checker
    /// must take the function's type from this annotation, not from the (diverging) body value.
    pub anon_fun_ret: std::collections::HashMap<u32, TypeRef>,
    /// `typealias Name = Target` — maps alias simple name → target simple name.
    /// Generic type aliases are stored with the raw target name (type args erased).
    pub type_aliases: Vec<(String, String)>,
    /// `typealias Name<T…> = (A) -> R` — aliases whose target is a FUNCTION type: the alias name,
    /// its declared type-parameter names (empty for a non-generic alias), and the full target
    /// `TypeRef` (parameters, return, `suspend`, receiver). A generic alias expands by substituting
    /// the use site's type arguments for the parameter names in a clone of the target.
    pub type_alias_fun: Vec<(String, Vec<String>, TypeRef)>,
    /// File-level annotations (`@file:Foo(args…)`) as `(simple_name, arg ExprIds)`. Lets a plugin read
    /// e.g. `@file:UseContextualSerialization(MyDate::class)` to mark matching property types contextual.
    pub file_annotations: Vec<(String, Vec<ExprId>)>,
    /// `ExprId`s of call arguments written with the spread operator (`*arr`). The marked id is the
    /// inner expression (the `arr` of `*arr`), which is what appears in the call's `args`. Lets the
    /// vararg lowering pass the array through (`Arrays.copyOf`) instead of packing it as one element.
    pub spread_arg_ids: std::collections::HashSet<u32>,
    /// Annotations written on a TYPE (`@Composable () -> Unit`, `@UnsafeVariance T`), keyed by the
    /// type's start offset (`TypeRef.span.lo`). The parser consumes leading `@Foo` before a type and
    /// records the simple names here; a plugin recovers them via the type's span (e.g. to detect a
    /// composable function type) without bloating every `TypeRef`. Absent ⇒ the type had no annotations.
    pub type_annotations: std::collections::HashMap<u32, Vec<String>>,
    /// Original projected argument keyed by the enclosing type's start offset.
    pub type_projection_args: std::collections::HashMap<u32, TypeRef>,
    /// `// ASSERTIONS_MODE: always-enable` — `assert(...)` is emitted UNGUARDED (always checks + throws),
    /// not behind the per-class `desiredAssertionStatus()` guard. From the test directive / `-Xassertions`.
    pub assert_always_enabled: bool,
    /// `// ASSERTIONS_MODE: always-disable` — `assert(...)` is elided entirely (the condition is not even
    /// evaluated). Mutually exclusive with `assert_always_enabled`; both unset ⇒ the per-class guard.
    pub assert_always_disabled: bool,
}

fn retain_default_span(
    default: Option<ExprId>,
    expr_spans: &[Span],
    retained: &mut std::collections::HashMap<ExprId, Span>,
) {
    let Some(default) = default else {
        return;
    };
    if let Some(&span) = expr_spans.get(default.0 as usize) {
        retained.insert(default, span);
    }
}

fn retain_param_default_spans(
    params: &[Param],
    expr_spans: &[Span],
    retained: &mut std::collections::HashMap<ExprId, Span>,
) {
    for param in params {
        retain_default_span(param.default, expr_spans, retained);
    }
}

fn retain_class_default_spans(
    class: &ClassDecl,
    expr_spans: &[Span],
    retained: &mut std::collections::HashMap<ExprId, Span>,
) {
    for param in &class.props {
        retain_default_span(param.default, expr_spans, retained);
    }
    for function in class.methods.iter().chain(&class.companion_methods) {
        retain_param_default_spans(&function.params, expr_spans, retained);
    }
    for constructor in &class.secondary_ctors {
        retain_param_default_spans(&constructor.params, expr_spans, retained);
    }
}

impl File {
    /// Release body arenas while retaining declaration metadata.
    pub fn release_body_arenas(&mut self) {
        let mut retained_expr_spans = std::collections::HashMap::new();
        for declaration in &self.decl_arena {
            match declaration {
                Decl::Fun(function) => retain_param_default_spans(
                    &function.params,
                    &self.expr_spans,
                    &mut retained_expr_spans,
                ),
                Decl::Class(class) => {
                    retain_class_default_spans(class, &self.expr_spans, &mut retained_expr_spans)
                }
                Decl::Property(_) => {}
            }
        }
        self.retained_expr_spans = retained_expr_spans;
        self.script_body = None;
        self.expr_arena = Vec::new();
        self.stmt_arena = Vec::new();
        self.expr_spans = Vec::new();
        self.stmt_spans = Vec::new();
        self.expr_lines = Vec::new();
        self.expr_source_lines = Vec::new();
        self.expr_end_lines = Vec::new();
        self.stmt_lines = Vec::new();
        self.value_operator_spans = Default::default();
        self.assignment_target_spans = Default::default();
        self.call_arg_names = Default::default();
        self.call_arg_name_spans = Default::default();
        self.empty_call_open_paren_spans = Default::default();
        self.exact_member_name_spans = Default::default();
        self.non_adjacent_member_dot_spans = Default::default();
        self.call_has_trailing_lambda = Default::default();
        self.trailing_call_close_paren_ends = Default::default();
        self.infix_calls = Default::default();
        self.call_type_args = Default::default();
        self.anonymous_object_classes = Default::default();
        self.local_class_decls = Default::default();
        self.lambda_param_types = Default::default();
        self.anon_fun_lambdas = Default::default();
        self.suspend_lambdas = Default::default();
        self.lambda_labels = Default::default();
        self.destructure_source_props = Default::default();
        self.base_arg_names = Default::default();
        self.anon_fun_ret = Default::default();
        self.file_annotations = Default::default();
        self.spread_arg_ids = Default::default();
    }

    pub fn expr_span(&self, id: ExprId) -> Option<Span> {
        self.expr_spans
            .get(id.0 as usize)
            .copied()
            .or_else(|| self.retained_expr_spans.get(&id).copied())
    }

    pub fn is_local_declaration(&self, id: DeclId) -> bool {
        self.local_declarations.contains(&id)
    }

    pub(crate) fn mark_local_declaration(&mut self, id: DeclId) {
        self.local_declarations.insert(id);
    }

    /// Whether call argument `id` (the inner expr of `*expr`) was written with the spread operator.
    pub fn is_spread_arg(&self, id: ExprId) -> bool {
        self.spread_arg_ids.contains(&id.0)
    }

    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.expr_arena[id.0 as usize]
    }
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmt_arena[id.0 as usize]
    }
    pub fn decl(&self, id: DeclId) -> &Decl {
        &self.decl_arena[id.0 as usize]
    }

    pub fn decl_mut(&mut self, id: DeclId) -> &mut Decl {
        &mut self.decl_arena[id.0 as usize]
    }

    pub fn add_expr(&mut self, e: Expr, span: Span) -> ExprId {
        let id = ExprId(self.expr_arena.len() as u32);
        self.expr_arena.push(e);
        self.expr_spans.push(span);
        id
    }
    pub fn add_stmt(&mut self, s: Stmt, span: Span) -> StmtId {
        let id = StmtId(self.stmt_arena.len() as u32);
        self.stmt_arena.push(s);
        self.stmt_spans.push(span);
        id
    }
    pub fn add_decl(&mut self, d: Decl) -> DeclId {
        let id = DeclId(self.decl_arena.len() as u32);
        self.decl_arena.push(d);
        id
    }

    /// Evaluate the narrow AST-only string-constant language shared by lowering and native plugins.
    ///
    /// Keeping this traversal on [`File`] gives both consumers one definition of which syntax is
    /// accepted: string literals, `Char` literals that have a Unicode-scalar spelling, templates made
    /// entirely from accepted parts, and top-level property references whose initializer is accepted.
    /// This is deliberately not a general Kotlin constant evaluator; unsupported syntax returns
    /// `None`, and each caller owns its own fallback or diagnostic policy.
    ///
    /// The result is a [`KtString`], not a Rust `String`: a `Char` part may be a UTF-16 code unit
    /// with no Unicode-scalar form (`'\uD800'`), and a string literal may already carry one, so the
    /// value can only be spelled as code units. Recursion through property references is bounded so
    /// malformed or cyclic input cannot overflow the compiler stack.
    pub(crate) fn const_string_value(&self, expression: ExprId) -> Option<KtString> {
        self.const_string_value_at_depth(expression, 0)
    }

    fn const_string_value_at_depth(&self, expression: ExprId, depth: u32) -> Option<KtString> {
        if depth > 32 {
            return None;
        }
        match self.expr(expression) {
            Expr::StringLit(value) => Some(value.clone()),
            Expr::CharLit(unit) => {
                let mut value = KtStringBuf::new();
                value.push_unit(*unit);
                Some(value.finish())
            }
            Expr::Name(name) => self.top_level_const_string_at_depth(name, depth + 1),
            Expr::Template(parts) => {
                let mut value = KtStringBuf::new();
                for part in parts {
                    match part {
                        TemplatePart::Str(text) => value.push_kt(text),
                        TemplatePart::Expr(expression) => value
                            .push_kt(&self.const_string_value_at_depth(*expression, depth + 1)?),
                    }
                }
                Some(value.finish())
            }
            _ => None,
        }
    }

    fn top_level_const_string_at_depth(&self, name: &str, depth: u32) -> Option<KtString> {
        if depth > 32 {
            return None;
        }
        self.decls
            .iter()
            .find_map(|&declaration| match self.decl(declaration) {
                Decl::Property(property) if property.name == name => {
                    property.init.and_then(|initializer| {
                        self.const_string_value_at_depth(initializer, depth + 1)
                    })
                }
                _ => None,
            })
    }

    /// Whether `declaration` is the synthetic class structurally owned by an anonymous-object
    /// construction in this file.
    ///
    /// Consumers must use declaration identity rather than inspecting the parser's generated class
    /// name. The name is an emission detail and may contain source-derived or sequence text; this map
    /// is the AST's canonical ownership relation and cannot confuse an ordinary user class that happens
    /// to resemble a synthetic naming convention.
    pub fn is_anonymous_object_class(&self, declaration: DeclId) -> bool {
        self.anonymous_object_classes
            .values()
            .any(|candidate| *candidate == declaration)
    }

    /// Whether the predicate accepts any expression root structurally owned by `declaration`.
    ///
    /// This is the declaration-level counterpart to [`Self::any_child_expr`]: it is the single
    /// inventory of expression-bearing declaration fields. Callers decide how (or whether) to walk
    /// below each returned root, so capture analysis, source tooling, and future structural queries
    /// do not each grow their own class/function/property field list as the AST evolves.
    ///
    /// Nested member declarations are included. A local class stored in a statement is deliberately
    /// not descended here: parser normalization also hoists that class into `File::decls`, where it
    /// is visited as an isolated declaration just like the semantic checker visits it. No evaluation
    /// order is promised; this method describes containment only and may short-circuit.
    pub fn any_decl_expr(
        &self,
        declaration: DeclId,
        predicate: &mut impl FnMut(ExprId) -> bool,
    ) -> bool {
        match self.decl(declaration) {
            Decl::Fun(function) => any_fun_decl_expr(function, predicate),
            Decl::Class(class) => any_class_decl_expr(class, predicate),
            Decl::Property(property) => any_property_decl_expr(property, predicate),
        }
    }

    /// Whether the predicate accepts any expression root structurally owned by `function`.
    ///
    /// This narrower companion to [`Self::any_decl_expr`] deliberately reuses the same generic
    /// function inventory. Passes that need function-level ownership (rather than only top-level
    /// declaration ownership) therefore include parameter defaults and annotation arguments as
    /// well as the body without recreating that field list in resolver-specific code.
    pub fn any_fun_expr(
        &self,
        function: &FunDecl,
        predicate: &mut impl FnMut(ExprId) -> bool,
    ) -> bool {
        any_fun_decl_expr(function, predicate)
    }

    /// Whether any *direct* child expression or child statement of `e` satisfies the given predicate
    /// — the single structural definition of "what an expression contains", with `||`/`.any()`
    /// short-circuiting. Tree walks (free-variable / capture / `try` / `break`-context checks)
    /// delegate their uniform recursion here, overriding only the variants whose handling differs
    /// (scope boundaries, leaf checks); a new `Expr` variant is then covered by adding one arm
    /// *here*, not in every walker.
    pub fn any_child_expr(
        &self,
        e: ExprId,
        fe: &mut impl FnMut(ExprId) -> bool,
        fs: &mut impl FnMut(StmtId) -> bool,
    ) -> bool {
        match self.expr(e) {
            Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::UIntLit(_)
            | Expr::ULongLit(_)
            | Expr::DoubleLit(_)
            | Expr::FloatLit(_)
            | Expr::BoolLit(_)
            | Expr::StringLit(_)
            | Expr::CharLit(_)
            | Expr::NullLit
            | Expr::Break { .. }
            | Expr::Continue { .. }
            | Expr::Name(_) => false,
            Expr::CallableRef { receiver, .. } => receiver.map_or(false, |r| fe(r)),
            Expr::Return { value, .. } => match value {
                Some(v) => fe(*v),
                None => false,
            },
            Expr::NotNull { operand }
            | Expr::Throw { operand }
            | Expr::Unary { operand, .. }
            | Expr::Is { operand, .. }
            | Expr::As { operand, .. }
            | Expr::Lambda { body: operand, .. } => fe(*operand),
            Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => fe(*lhs) || fe(*rhs),
            Expr::RangeTo { lo, hi, .. } => fe(*lo) || fe(*hi),
            Expr::IncDec { target, .. } => fe(*target),
            Expr::InRange {
                value, start, end, ..
            } => fe(*value) || fe(*start) || fe(*end),
            Expr::Member { receiver, .. } => fe(*receiver),
            Expr::Index { array, indices } => fe(*array) || indices.iter().any(|&i| fe(i)),
            Expr::Call { callee, args } => fe(*callee) || args.iter().any(|&a| fe(a)),
            Expr::SafeCall { receiver, args, .. } => {
                fe(*receiver) || args.as_ref().map_or(false, |a| a.iter().any(|&x| fe(x)))
            }
            Expr::Template(parts) => parts
                .iter()
                .any(|p| matches!(p, TemplatePart::Expr(x) if fe(*x))),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => fe(*cond) || fe(*then_branch) || else_branch.map_or(false, |x| fe(x)),
            Expr::Block { stmts, trailing } => {
                stmts.iter().any(|&s| fs(s)) || trailing.map_or(false, |t| fe(t))
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                fe(*body) || catches.iter().any(|c| fe(c.body)) || finally.map_or(false, |f| fe(f))
            }
            Expr::When { subject, arms } => {
                subject.map_or(false, |s| fe(s))
                    || arms
                        .iter()
                        .any(|a| a.conditions.iter().any(|&c| fe(c)) || fe(a.body))
            }
        }
    }

    /// Whether any direct child expression of statement `s` satisfies the predicate. (A statement
    /// never directly contains another statement — nesting goes through a `Block` expression, handled
    /// by [`any_child_expr`](Self::any_child_expr).) Companion to that method.
    pub fn any_child_stmt(&self, s: StmtId, fe: &mut impl FnMut(ExprId) -> bool) -> bool {
        match self.stmt(s) {
            Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::Return(None, _)
            | Stmt::IncDec { .. }
            | Stmt::LocalLateinit { .. } => false,
            Stmt::Local { init, .. }
            | Stmt::Destructure { init, .. }
            | Stmt::Assign { value: init, .. }
            | Stmt::LocalDelegate { delegate: init, .. }
            | Stmt::Return(Some(init), _)
            | Stmt::Expr(init) => fe(*init),
            Stmt::AssignMember {
                receiver, value, ..
            } => fe(*receiver) || fe(*value),
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => fe(*array) || indices.iter().any(|&i| fe(i)) || fe(*value),
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                fe(*cond) || fe(*body)
            }
            Stmt::For { range, body, .. } => fe(range.start) || fe(range.end) || fe(*body),
            Stmt::ForEach { iterable, body, .. } => fe(*iterable) || fe(*body),
            Stmt::LocalFun(f) => matches!(&f.body, FunBody::Expr(b) | FunBody::Block(b) if fe(*b)),
            // A local class's members are hoisted + walked separately; it has no inline child expr here.
            Stmt::LocalClass(_) => false,
        }
    }

    pub fn expr_uses_name(&self, e: ExprId, name: &str) -> bool {
        let names: std::collections::HashSet<&str> = std::iter::once(name).collect();
        expr_refs_name(self, e, &names)
    }

    pub fn expr_uses_any_name(&self, e: ExprId, names: &std::collections::HashSet<&str>) -> bool {
        expr_refs_name(self, e, names)
    }

    pub fn expr_uses_name_deep(&self, e: ExprId, name: &str) -> bool {
        let names: std::collections::HashSet<&str> = std::iter::once(name).collect();
        expr_refs_name_inner(self, e, &names, true)
    }

    pub fn expr_uses_any_name_deep(
        &self,
        e: ExprId,
        names: &std::collections::HashSet<&str>,
    ) -> bool {
        expr_refs_name_inner(self, e, names, true)
    }
}

fn any_fun_body_expr(body: &FunBody, predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    match body {
        FunBody::Expr(expression) | FunBody::Block(expression) => predicate(*expression),
        FunBody::None => false,
    }
}

fn any_param_expr(params: &[Param], predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    for parameter in params {
        if parameter.default.is_some_and(&mut *predicate)
            || parameter
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .any(&mut *predicate)
        {
            return true;
        }
    }
    false
}

fn any_fun_decl_expr(function: &FunDecl, predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    any_param_expr(&function.params, predicate) || any_fun_body_expr(&function.body, predicate)
}

fn any_property_decl_expr(property: &PropDecl, predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    any_param_expr(&property.context_params, predicate)
        || property.init.is_some_and(&mut *predicate)
        || property.delegate.is_some_and(&mut *predicate)
        || property
            .getter
            .as_ref()
            .is_some_and(|body| any_fun_body_expr(body, predicate))
        || property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
            .is_some_and(|body| any_fun_body_expr(body, predicate))
}

fn any_class_decl_expr(class: &ClassDecl, predicate: &mut impl FnMut(ExprId) -> bool) -> bool {
    if class
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .any(&mut *predicate)
        || class.props.iter().any(|parameter| {
            parameter.default.is_some_and(&mut *predicate)
                || parameter
                    .annotation_args
                    .iter()
                    .flatten()
                    .copied()
                    .any(&mut *predicate)
        })
        || class
            .companion_base_args
            .iter()
            .copied()
            .any(&mut *predicate)
        || class.base_args.iter().copied().any(&mut *predicate)
        || class
            .delegation_exprs
            .iter()
            .any(|(_, expression)| predicate(*expression))
        || class.init_order.iter().any(|step| match step {
            ClassInit::Block(body) => predicate(*body),
            // The corresponding `body_props` entry is visited below; following the index here
            // would report the same initializer twice.
            ClassInit::PropInit(_) => false,
        })
        || class.enum_entries.iter().any(|entry| {
            entry
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .chain(entry.args.iter().copied())
                .any(&mut *predicate)
        })
    {
        return true;
    }

    for constructor in &class.secondary_ctors {
        let delegation_args = match &constructor.delegation {
            CtorDelegation::None => &[][..],
            CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
        };
        if any_param_expr(&constructor.params, predicate)
            || delegation_args.iter().copied().any(&mut *predicate)
            || constructor.body.is_some_and(&mut *predicate)
        {
            return true;
        }
    }

    class
        .methods
        .iter()
        .chain(&class.companion_methods)
        .chain(
            class
                .enum_entries
                .iter()
                .flat_map(|entry| entry.methods.iter()),
        )
        .any(|function| any_fun_decl_expr(function, predicate))
        || class
            .body_props
            .iter()
            .chain(&class.companion_props)
            .chain(
                class
                    .enum_entries
                    .iter()
                    .flat_map(|entry| entry.props.iter()),
            )
            .any(|property| any_property_decl_expr(property, predicate))
}

fn expr_refs_name(file: &File, e: ExprId, names: &std::collections::HashSet<&str>) -> bool {
    expr_refs_name_inner(file, e, names, false)
}

fn stmt_refs_name(
    file: &File,
    s: StmtId,
    names: &std::collections::HashSet<&str>,
    into_lambdas: bool,
) -> bool {
    match file.stmt(s) {
        Stmt::IncDec { name, .. } => names.contains(name.as_str()),
        Stmt::Assign { name, value } => {
            names.contains(name.as_str()) || expr_refs_name_inner(file, *value, names, into_lambdas)
        }
        Stmt::LocalFun(_) => false,
        _ => file.any_child_stmt(s, &mut |c| {
            expr_refs_name_inner(file, c, names, into_lambdas)
        }),
    }
}

fn expr_refs_name_inner(
    file: &File,
    e: ExprId,
    names: &std::collections::HashSet<&str>,
    into_lambdas: bool,
) -> bool {
    match file.expr(e) {
        Expr::Name(n) => names.contains(n.as_str()),
        Expr::Lambda { params, body } if into_lambdas => {
            let mut shadowed: std::collections::HashSet<&str> =
                params.iter().map(String::as_str).collect();
            if params.is_empty() {
                shadowed.insert("it");
            }
            let remaining: std::collections::HashSet<&str> =
                names.difference(&shadowed).copied().collect();
            !remaining.is_empty() && expr_refs_name_inner(file, *body, &remaining, true)
        }
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(
            e,
            &mut |c| expr_refs_name_inner(file, c, names, into_lambdas),
            &mut |s| stmt_refs_name(file, s, names, into_lambdas),
        ),
    }
}

// ---- S-expression debug printer (used by parser tests) ---------------------------------------

impl File {
    pub fn debug_tree(&self) -> String {
        let mut s = String::new();
        for &d in &self.decls {
            self.write_decl(d, &mut s);
            s.push('\n');
        }
        s
    }

    fn write_decl(&self, id: DeclId, out: &mut String) {
        match self.decl(id) {
            Decl::Property(p) => {
                out.push_str(&format!(
                    "({} {}",
                    if p.is_var { "var" } else { "val" },
                    p.name
                ));
                if let Some(t) = &p.ty {
                    out.push_str(&format!(" :{}", t.name));
                }
                out.push(' ');
                match p.init {
                    Some(i) => self.write_expr(i, out),
                    None => out.push_str("<lateinit>"),
                }
                out.push(')');
            }
            Decl::Class(c) if c.is_interface() => {
                out.push_str(&format!("(interface {}", c.name));
                for m in &c.methods {
                    out.push_str(&format!(" (absfun {})", m.name));
                }
                out.push(')');
            }
            Decl::Class(c) if c.is_enum() => {
                out.push_str(&format!("(enum {}", c.name));
                for e in &c.enum_entries {
                    out.push_str(&format!(" {}", e.name));
                }
                out.push(')');
            }
            Decl::Class(c) => {
                let keyword = match c.kind {
                    ClassKind::Object => "object",
                    ClassKind::Annotation => "annotation",
                    _ => "class",
                };
                out.push_str(&format!("({} {}", keyword, c.name));
                for p in &c.props {
                    out.push_str(&format!(
                        " ({} {} {})",
                        if p.is_var { "var" } else { "val" },
                        p.name,
                        p.ty.name
                    ));
                }
                for m in &c.methods {
                    out.push(' ');
                    let id = DeclId(u32::MAX); // not arena-backed; render inline
                    let _ = id;
                    out.push_str(&format!("(method {}", m.name));
                    for p in &m.params {
                        out.push_str(&format!(" (param {} {})", p.name, p.ty.name));
                    }
                    if let Some(r) = &m.ret {
                        out.push_str(&format!(" :{}", r.name));
                    }
                    out.push(')');
                }
                out.push(')');
            }
            Decl::Fun(f) => {
                out.push_str(&format!("(fun {}", f.name));
                for p in &f.params {
                    out.push_str(&format!(" (param {} {})", p.name, p.ty.name));
                }
                if let Some(r) = &f.ret {
                    out.push_str(&format!(" :{}", r.name));
                }
                out.push(' ');
                match &f.body {
                    FunBody::Expr(e) | FunBody::Block(e) => self.write_expr(*e, out),
                    FunBody::None => out.push_str("<none>"),
                }
                out.push(')');
            }
        }
    }

    fn write_expr(&self, id: ExprId, out: &mut String) {
        match self.expr(id) {
            Expr::IntLit(v) => out.push_str(&v.to_string()),
            Expr::LongLit(v) => out.push_str(&format!("{v}L")),
            Expr::UIntLit(v) => out.push_str(&format!("{v}u")),
            Expr::ULongLit(v) => out.push_str(&format!("{v}uL")),
            Expr::DoubleLit(v) => out.push_str(&format!("{v}d")),
            Expr::FloatLit(v) => out.push_str(&format!("{v}f")),
            Expr::BoolLit(b) => out.push_str(if *b { "true" } else { "false" }),
            Expr::StringLit(s) => out.push_str(&format!("{s:?}")),
            // A code unit that is not a scalar value (a lone surrogate) has no `char` to print.
            Expr::CharLit(c) => match char::from_u32(*c as u32) {
                Some(c) => out.push_str(&format!("'{c}'")),
                None => out.push_str(&format!("'\\u{c:04X}'")),
            },
            Expr::NullLit => out.push_str("null"),
            Expr::Name(n) => out.push_str(n),
            Expr::NotNull { operand } => {
                out.push_str("(!! ");
                self.write_expr(*operand, out);
                out.push(')');
            }
            Expr::Elvis { lhs, rhs } => {
                out.push_str("(?: ");
                self.write_expr(*lhs, out);
                out.push(' ');
                self.write_expr(*rhs, out);
                out.push(')');
            }
            Expr::Throw { operand } => {
                out.push_str("(throw ");
                self.write_expr(*operand, out);
                out.push(')');
            }
            Expr::Break { label } => {
                out.push_str("(break");
                if let Some(l) = label {
                    out.push_str(&format!("@{l}"));
                }
                out.push(')');
            }
            Expr::Continue { label } => {
                out.push_str("(continue");
                if let Some(l) = label {
                    out.push_str(&format!("@{l}"));
                }
                out.push(')');
            }
            Expr::Return { value, label } => {
                out.push_str("(return");
                if let Some(l) = label {
                    out.push_str(&format!("@{l}"));
                }
                if let Some(v) = value {
                    out.push(' ');
                    self.write_expr(*v, out);
                }
                out.push(')');
            }
            Expr::Lambda { params, body } => {
                out.push_str(&format!(
                    "(lambda {} ",
                    if params.is_empty() {
                        first_lambda_param_or_it(params)
                    } else {
                        params.join(",")
                    }
                ));
                self.write_expr(*body, out);
                out.push(')');
            }
            Expr::Index { array, indices } => {
                out.push_str(if indices.len() == 1 {
                    "(index "
                } else {
                    "(index-multi "
                });
                self.write_expr(*array, out);
                for &i in indices {
                    out.push(' ');
                    self.write_expr(i, out);
                }
                out.push(')');
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                out.push_str("(try ");
                self.write_expr(*body, out);
                for c in catches {
                    out.push_str(&format!(" catch {}:{} ", c.name, c.ty.name));
                    self.write_expr(c.body, out);
                }
                if let Some(f) = finally {
                    out.push_str(" finally ");
                    self.write_expr(*f, out);
                }
                out.push(')');
            }
            Expr::Is {
                operand,
                ty,
                negated,
            } => {
                out.push_str(if *negated { "(!is " } else { "(is " });
                self.write_expr(*operand, out);
                out.push_str(&format!(" {})", ty.name));
            }
            Expr::As {
                operand,
                ty,
                nullable,
            } => {
                out.push_str(if *nullable { "(as? " } else { "(as " });
                self.write_expr(*operand, out);
                out.push_str(&format!(" {})", ty.name));
            }
            Expr::InRange {
                value,
                start,
                end,
                kind,
                negated,
            } => {
                out.push_str(if *negated { "(!in " } else { "(in " });
                self.write_expr(*value, out);
                let op = match kind {
                    RangeKind::Through => "..",
                    RangeKind::Until => "until",
                    RangeKind::DownTo => "downTo",
                };
                out.push_str(&format!(" {op} "));
                self.write_expr(*start, out);
                out.push(' ');
                self.write_expr(*end, out);
                out.push(')');
            }
            Expr::RangeTo { lo, hi, kind } => {
                let op = match kind {
                    RangeKind::Through => "..",
                    RangeKind::Until => "..<",
                    RangeKind::DownTo => "downTo",
                };
                out.push_str(&format!("({op} "));
                self.write_expr(*lo, out);
                out.push(' ');
                self.write_expr(*hi, out);
                out.push(')');
            }
            Expr::IncDec {
                target,
                dec,
                prefix,
            } => {
                out.push_str(if *prefix { "(pre" } else { "(post" });
                out.push_str(if *dec { "-- " } else { "++ " });
                self.write_expr(*target, out);
                out.push(')');
            }
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                out.push_str("(?. ");
                self.write_expr(*receiver, out);
                out.push_str(&format!(" {name}"));
                if let Some(args) = args {
                    for a in args {
                        out.push(' ');
                        self.write_expr(*a, out);
                    }
                }
                out.push(')');
            }
            Expr::Template(parts) => {
                out.push_str("(template");
                for p in parts {
                    match p {
                        TemplatePart::Str(s) => out.push_str(&format!(" {s:?}")),
                        TemplatePart::Expr(e) => {
                            out.push(' ');
                            self.write_expr(*e, out);
                        }
                    }
                }
                out.push(')');
            }
            Expr::Unary { op, operand } => {
                out.push_str(&format!("({} ", unop(*op)));
                self.write_expr(*operand, out);
                out.push(')');
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                out.push_str(&format!("({} ", binop(*op)));
                self.write_expr(*lhs, out);
                out.push(' ');
                self.write_expr(*rhs, out);
                out.push(')');
            }
            Expr::Member { receiver, name } => {
                out.push_str("(. ");
                self.write_expr(*receiver, out);
                out.push_str(&format!(" {name})"));
            }
            Expr::Call { callee, args } => {
                out.push_str("(call ");
                self.write_expr(*callee, out);
                for a in args {
                    out.push(' ');
                    self.write_expr(*a, out);
                }
                out.push(')');
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                out.push_str("(if ");
                self.write_expr(*cond, out);
                out.push(' ');
                self.write_expr(*then_branch, out);
                if let Some(e) = else_branch {
                    out.push(' ');
                    self.write_expr(*e, out);
                }
                out.push(')');
            }
            Expr::When { subject, arms } => {
                out.push_str("(when");
                if let Some(s) = subject {
                    out.push(' ');
                    self.write_expr(*s, out);
                }
                for arm in arms {
                    out.push_str(" (arm");
                    for cnd in &arm.conditions {
                        out.push(' ');
                        self.write_expr(*cnd, out);
                    }
                    if arm.conditions.is_empty() {
                        out.push_str(" else");
                    }
                    out.push_str(" => ");
                    self.write_expr(arm.body, out);
                    out.push(')');
                }
                out.push(')');
            }
            Expr::Block { stmts, trailing } => {
                out.push_str("(block");
                for s in stmts {
                    out.push(' ');
                    self.write_stmt(*s, out);
                }
                if let Some(e) = trailing {
                    out.push_str(" =>");
                    self.write_expr(*e, out);
                }
                out.push(')');
            }
            Expr::CallableRef { receiver, name } => {
                if let Some(r) = receiver {
                    self.write_expr(*r, out);
                }
                out.push_str(&format!("::{name}"));
            }
        }
    }

    fn write_stmt(&self, id: StmtId, out: &mut String) {
        match self.stmt(id) {
            Stmt::Local {
                is_var, name, init, ..
            } => {
                out.push_str(&format!("({} {name} ", if *is_var { "var" } else { "val" }));
                self.write_expr(*init, out);
                out.push(')');
            }
            Stmt::LocalLateinit { name, .. } => {
                out.push_str(&format!("(lateinit var {name})"));
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                delegate,
                ..
            } => {
                out.push_str(&format!(
                    "({} {name} by ",
                    if *is_var { "var" } else { "val" }
                ));
                self.write_expr(*delegate, out);
                out.push(')');
            }
            Stmt::Destructure { entries, init } => {
                let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
                out.push_str(&format!("(destructure ({}) ", names.join(" ")));
                self.write_expr(*init, out);
                out.push(')');
            }
            Stmt::Assign { name, value } => {
                out.push_str(&format!("(set {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::IncDec { name, dec, .. } => {
                out.push_str(&format!("({} {name})", if *dec { "dec" } else { "inc" }));
            }
            Stmt::AssignMember {
                receiver,
                name,
                value,
            } => {
                out.push_str("(set-member ");
                self.write_expr(*receiver, out);
                out.push_str(&format!(" {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => {
                out.push_str(if indices.len() == 1 {
                    "(set-index "
                } else {
                    "(set-index-multi "
                });
                self.write_expr(*array, out);
                for &i in indices {
                    out.push(' ');
                    self.write_expr(i, out);
                }
                out.push(' ');
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::Break(l) => out.push_str(&format!(
                "(break{})",
                l.as_ref().map(|s| format!("@{s}")).unwrap_or_default()
            )),
            Stmt::Continue(l) => out.push_str(&format!(
                "(continue{})",
                l.as_ref().map(|s| format!("@{s}")).unwrap_or_default()
            )),
            Stmt::Return(e, label) => {
                out.push_str("(return");
                if let Some(l) = label {
                    out.push_str(&format!("@{l}"));
                }
                if let Some(e) = e {
                    out.push(' ');
                    self.write_expr(*e, out);
                }
                out.push(')');
            }
            Stmt::While { cond, body, .. } => {
                out.push_str("(while ");
                self.write_expr(*cond, out);
                out.push(' ');
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::DoWhile { body, cond, .. } => {
                out.push_str("(do ");
                self.write_expr(*body, out);
                out.push_str(" while ");
                self.write_expr(*cond, out);
                out.push(')');
            }
            Stmt::For {
                name, range, body, ..
            } => {
                let op = match range.kind {
                    crate::ast::RangeKind::Through => "..",
                    crate::ast::RangeKind::Until => "until",
                    crate::ast::RangeKind::DownTo => "downTo",
                };
                out.push_str(&format!("(for {name} ("));
                self.write_expr(range.start, out);
                out.push_str(&format!(" {op} "));
                self.write_expr(range.end, out);
                out.push_str(") ");
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                out.push_str(&format!("(for-each {name} "));
                self.write_expr(*iterable, out);
                out.push(' ');
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::Expr(e) => self.write_expr(*e, out),
            Stmt::LocalFun(f) => {
                out.push_str(&format!("(local-fun {})", f.name));
            }
            Stmt::LocalClass(c) => {
                out.push_str(&format!("(local-class {})", c.name));
            }
        }
    }
}

fn binop(op: BinOp) -> &'static str {
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
fn unop(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
        UnOp::Plus => "plus",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span::new(0, 0)
    }

    #[test]
    fn binary_operator_location_does_not_widen_the_expression_arena_entry() {
        assert!(
            std::mem::size_of::<Expr>() <= 104,
            "Expr grew to {} bytes",
            std::mem::size_of::<Expr>()
        );
    }

    #[test]
    fn expr_uses_name_stops_at_nested_lambdas() {
        let mut file = File::default();
        let outer = file.add_expr(Expr::Name("outer".to_string()), span());
        let lambda = file.add_expr(
            Expr::Lambda {
                params: Vec::new(),
                body: outer,
            },
            span(),
        );

        assert!(!file.expr_uses_name(lambda, "outer"));
        assert!(file.expr_uses_name_deep(lambda, "outer"));
    }

    #[test]
    fn expr_uses_name_deep_respects_lambda_parameter_shadowing() {
        let mut file = File::default();
        let outer = file.add_expr(Expr::Name("outer".to_string()), span());
        let lambda = file.add_expr(
            Expr::Lambda {
                params: vec!["outer".to_string()],
                body: outer,
            },
            span(),
        );

        assert!(!file.expr_uses_name_deep(lambda, "outer"));
    }

    #[test]
    fn expr_uses_name_counts_assignment_targets_and_values() {
        let mut file = File::default();
        let value = file.add_expr(Expr::Name("value".to_string()), span());
        let assign = file.add_stmt(
            Stmt::Assign {
                name: "target".to_string(),
                value,
            },
            span(),
        );
        let block = file.add_expr(
            Expr::Block {
                stmts: vec![assign],
                trailing: None,
            },
            span(),
        );

        assert!(file.expr_uses_name(block, "target"));
        assert!(file.expr_uses_name(block, "value"));
        assert!(!file.expr_uses_name(block, "missing"));
    }

    #[test]
    fn const_string_value_renders_a_char_as_its_code_unit() {
        let mut file = File::default();
        let dollar = file.add_expr(Expr::CharLit(b'$' as u16), span());
        let surrogate = file.add_expr(Expr::CharLit(0xD800), span());

        assert_eq!(file.const_string_value(dollar), Some(KtString::from("$")));
        // A lone surrogate is a legal `Char` with no Unicode-scalar form. It must survive the fold
        // as its code unit rather than make the whole constant unrepresentable.
        let folded = file.const_string_value(surrogate).expect("lone surrogate");
        assert_eq!(folded.units().collect::<Vec<_>>(), vec![0xD800]);
        assert_eq!(folded.as_str(), None);
    }

    #[test]
    fn const_string_value_joins_a_template_in_code_units() {
        // The two halves of U+1F600 arriving as separate `Char` parts must rejoin into the ordinary
        // two-unit string, not stay in the degraded form.
        let mut file = File::default();
        let high = file.add_expr(Expr::CharLit(0xD83D), span());
        let low = file.add_expr(Expr::CharLit(0xDE00), span());
        let template = file.add_expr(
            Expr::Template(vec![
                TemplatePart::Str(KtString::from("[")),
                TemplatePart::Expr(high),
                TemplatePart::Expr(low),
                TemplatePart::Str(KtString::from("]")),
            ]),
            span(),
        );

        assert_eq!(
            file.const_string_value(template),
            Some(KtString::from("[\u{1F600}]"))
        );
    }
}
