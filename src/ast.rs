//! Index-based arena AST (data-oriented: no `Box`/`Rc` graph, all edges are `u32` ids into
//! parallel `Vec`s, so a file's whole AST is one bulk-freeable allocation block).

use crate::diag::Span;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ExprId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct StmtId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct DeclId(pub u32);

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
    /// Unary `+` — identity on the built-in numeric types (`+x == x`); a user `unaryPlus` operator is
    /// not modeled (the lowerer bails on a non-numeric operand).
    Plus,
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
    StringLit(String),
    CharLit(char),
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
    },
    /// `receiver.name` (no call). For a bare name use `Name`.
    Member {
        receiver: ExprId,
        name: String,
    },
    /// `array[index]` — array element read.
    Index {
        array: ExprId,
        index: ExprId,
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
    Str(String),
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
    },
    /// `receiver.name = value` — write a (mutable) property via its setter.
    AssignMember {
        receiver: ExprId,
        name: String,
        value: ExprId,
    },
    /// `array[index] = value` — array element store.
    AssignIndex {
        array: ExprId,
        index: ExprId,
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

/// A syntactic type reference. v0: just a simple name (`Int`, `String`, ...).
#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: String,
    /// Trailing `?` — a nullable type (e.g. `String?`).
    pub nullable: bool,
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
    /// For a receiver function type `Recv.(A) -> R`: `true`, and `fun_params[0]` is the receiver
    /// `Recv`. The receiver folds in as the first `FunctionN` parameter (matching Kotlin's lowering),
    /// but the front end keeps this marker so a lambda passed to such a param binds `fun_params[0]`
    /// as the implicit `this` receiver (member access, and an arity-short `f()` supplies it).
    pub fun_has_receiver: bool,
    /// For a `suspend` function type `suspend (A) -> R`: `true`. Lowers to `Function{n+1}` with a
    /// trailing `kotlin/coroutines/Continuation` parameter and an `Object`-erased result (kotlinc's
    /// suspend-lambda ABI), distinct from the plain `Function{n}` of a non-suspend function type.
    pub fun_suspend: bool,
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

#[derive(Clone, Debug)]
pub struct FunDecl {
    pub name: String,
    /// Extension receiver type (`fun String.foo()` → `Some("String")`). Emitted as a static
    /// method with the receiver prepended as the first parameter.
    pub receiver: Option<TypeRef>,
    pub params: Vec<Param>,
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
    pub is_inline: bool,
    /// `final` modifier — cannot be overridden. Data-class synthesis skips methods a parent marks
    /// `final` (overriding them would produce wrong behavior).
    pub is_final: bool,
    /// `abstract` modifier — a member with no body, only valid in an abstract class or interface.
    pub is_abstract: bool,
    /// `private` visibility. Public/internal/protected functions get `Intrinsics.checkNotNullParameter`
    /// guards on their non-null reference parameters (kotlinc does); private ones do not.
    pub is_private: bool,
    /// `suspend` modifier — a coroutine. Lowered continuation-passing-style: an extra
    /// `kotlin.coroutines.Continuation` parameter is appended and the return type erases to
    /// `java.lang.Object` (a leaf function with no suspension point needs no state machine).
    pub is_suspend: bool,
    /// `tailrec` modifier — a self-recursive function whose tail calls the lowerer rewrites into a loop
    /// (param reassignment + `continue`), so deep recursion doesn't overflow the stack.
    pub is_tailrec: bool,
    /// Simple names of annotations applied to this function (`@Composable fun f()` → `["Composable"]`),
    /// mirroring `ClassDecl.annotations`. Used by the compiler-extension surface (`crate::plugins`) to
    /// find annotated functions.
    pub annotations: Vec<String>,
}

/// A primary-constructor parameter that is also a property (`val`/`var name: Type`).
/// v0: property types are restricted to the primitive/String `Ty` set (no class-typed members yet).
#[derive(Clone, Debug)]
pub struct PropParam {
    pub name: String,
    pub ty: TypeRef,
    pub is_var: bool,
    /// `true` for a `val`/`var` parameter (a property → backing field + accessor); `false` for a
    /// plain constructor parameter (in scope for `init`/body-property initializers, but not a field).
    pub is_property: bool,
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
}

/// One entry of an `enum class` (`RED(0xFF0000) { override fun m() = … }`). Groups what were parallel
/// `Vec`s keyed by entry index (name / constructor args / per-entry-body methods / per-entry-body
/// properties), so an entry's four facets can't desync.
#[derive(Clone, Debug)]
pub struct AstEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Constructor arguments (`RED(0xFF0000)` → the two arg expr ids); empty for `RED` with no args.
    pub args: Vec<ExprId>,
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
    pub base_args: Vec<ExprId>,
    /// Secondary constructors: `constructor(params) : this/super(args) { body }`.
    pub secondary_ctors: Vec<SecondaryCtor>,
    /// `false` when the class declares NO primary constructor (`class A { constructor(...) }`): every
    /// constructor is a secondary, and a `super(...)`/implicit-delegating one (not `this(...)`) runs the
    /// field initializers + `init {}` blocks. `true` for an implicit/explicit primary (`class A`,
    /// `class A()`, `class A(...)`), including a `class A() { constructor(...) : this(...) }`.
    pub has_primary_ctor: bool,
    pub span: Span,
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
    This(Vec<ExprId>),
    Super(Vec<ExprId>),
}

/// A primary-constructor init step (source-ordered): a body-property initializer or an `init` block.
#[derive(Clone, Debug)]
pub enum ClassInit {
    PropInit(usize), // index into ClassDecl.body_props
    Block(ExprId),   // an `init { … }` block expression
}

/// A top-level `val`/`var` property: `val name: Type = init`.
#[derive(Clone, Debug)]
pub struct PropDecl {
    pub name: String,
    /// Extension-property receiver type (`val String.foo: T` → `Some("String")`). The getter/setter
    /// are emitted as static `getFoo(Recv)`/`setFoo(Recv, T)` methods, like an extension function.
    pub receiver: Option<TypeRef>,
    pub ty: Option<TypeRef>,
    pub is_var: bool,
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
    /// Fully-qualified import names (e.g. `util.Calc`), used to resolve Java/JDK references.
    pub imports: Vec<String>,
    pub decls: Vec<DeclId>,
    pub decl_arena: Vec<Decl>,
    pub expr_arena: Vec<Expr>,
    pub stmt_arena: Vec<Stmt>,
    pub expr_spans: Vec<Span>,
    pub stmt_spans: Vec<Span>,
    /// Per-`Expr::Call` argument names: keyed by the call's `ExprId`, parallel to its `args`
    /// (`None` = positional, `Some(name)` = `name = expr`). Absent ⇒ all positional.
    pub call_arg_names: std::collections::HashMap<u32, Vec<Option<String>>>,
    /// `ExprId`s of `Expr::Call`s whose LAST argument is a SYNTACTIC trailing lambda (`f(a) { … }` /
    /// `f { … }`). A trailing lambda always binds to the callee's LAST parameter — preceding parameters
    /// without a positional argument take their defaults — so default-omission lowering must place it in
    /// the last slot, not the next free positional one (`f("x") { }` on `f(a, m = d, builder)` ⇒ `m`
    /// defaults, the lambda fills `builder`).
    pub call_has_trailing_lambda: std::collections::HashSet<u32>,
    /// `ExprId`s of `Expr::Call`s produced from infix-call syntax (`a foo b`). The callee is still the
    /// ordinary `Member { receiver: a, name: "foo" }`, but resolver/lowering need the source form for
    /// primitive builtin names where Kotlin treats `a rem b` differently from `a.rem(b)`.
    pub infix_calls: std::collections::HashSet<u32>,
    /// Explicit type arguments on a call (`Foo<Int>()`, `listOf<String>(…)`), keyed by the call's
    /// `ExprId`. Lets a constructor call carry its instantiation (`ArrayList<Int>()` → `ArrayList<Int>`)
    /// so member/element types resolve. Absent ⇒ no explicit type arguments.
    pub call_type_args: std::collections::HashMap<u32, Vec<TypeRef>>,
    /// Explicit parameter type annotations on a lambda literal (`{ x: Int, y -> … }`), keyed by the
    /// lambda's `ExprId`, parallel to its `params`. `None` for an unannotated parameter. Lets the
    /// checker type a *bare-value* lambda (`val f = { x: Int -> x*2 }`) from its own declared types
    /// when no expected function type drives them.
    pub lambda_param_types: std::collections::HashMap<u32, Vec<Option<TypeRef>>>,
    /// `typealias Name = Target` — maps alias simple name → target simple name.
    /// Generic type aliases are stored with the raw target name (type args erased).
    pub type_aliases: Vec<(String, String)>,
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
    /// `// ASSERTIONS_MODE: always-enable` — `assert(...)` is emitted UNGUARDED (always checks + throws),
    /// not behind the per-class `desiredAssertionStatus()` guard. From the test directive / `-Xassertions`.
    pub assert_always_enabled: bool,
    /// `// ASSERTIONS_MODE: always-disable` — `assert(...)` is elided entirely (the condition is not even
    /// evaluated). Mutually exclusive with `assert_always_enabled`; both unset ⇒ the per-class guard.
    pub assert_always_disabled: bool,
}

impl File {
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
            Expr::Index { array, index } => fe(*array) || fe(*index),
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
                index,
                value,
            } => fe(*array) || fe(*index) || fe(*value),
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
            Expr::CharLit(c) => out.push_str(&format!("'{c}'")),
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
                        "it".to_string()
                    } else {
                        params.join(",")
                    }
                ));
                self.write_expr(*body, out);
                out.push(')');
            }
            Expr::Index { array, index } => {
                out.push_str("(index ");
                self.write_expr(*array, out);
                out.push(' ');
                self.write_expr(*index, out);
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
            Expr::Binary { op, lhs, rhs } => {
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
            Stmt::IncDec { name, dec } => {
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
                index,
                value,
            } => {
                out.push_str("(set-index ");
                self.write_expr(*array, out);
                out.push(' ');
                self.write_expr(*index, out);
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

    fn sp() -> Span {
        Span::new(0, 0)
    }

    fn tref(name: &str) -> TypeRef {
        TypeRef {
            name: name.to_string(),
            nullable: false,
            arg: None,
            targs: Vec::new(),
            span: sp(),
            fun_params: Vec::new(),
            fun_has_receiver: false,
            fun_suspend: false,
        }
    }

    fn class_decl(name: &str, kind: ClassKind, modality: Modality) -> ClassDecl {
        ClassDecl {
            name: name.to_string(),
            annotations: Vec::new(),
            annotation_args: Vec::new(),
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            props: Vec::new(),
            methods: Vec::new(),
            companion_methods: Vec::new(),
            companion_props: Vec::new(),
            companion_base: None,
            companion_base_args: Vec::new(),
            companion_supertypes: Vec::new(),
            body_props: Vec::new(),
            init_order: Vec::new(),
            kind,
            is_data: false,
            is_value: false,
            enum_entries: Vec::new(),
            is_fun_interface: false,
            modality,
            inner_of: None,
            supertypes: Vec::new(),
            delegations: Vec::new(),
            delegation_exprs: Vec::new(),
            base_class: None,
            base_args: Vec::new(),
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            span: sp(),
        }
    }

    // ---- BinOp / UnOp helpers ------------------------------------------------

    #[test]
    fn arith_operator_name_roundtrips() {
        let pairs = [
            (BinOp::Add, "plus"),
            (BinOp::Sub, "minus"),
            (BinOp::Mul, "times"),
            (BinOp::Div, "div"),
            (BinOp::Rem, "rem"),
        ];
        for (op, name) in pairs {
            assert_eq!(op.arith_operator_name(), Some(name));
            assert_eq!(BinOp::from_arith_operator_name(name), Some(op));
        }
    }

    #[test]
    fn arith_operator_name_none_for_non_arith() {
        for op in [
            BinOp::Eq,
            BinOp::Ne,
            BinOp::Lt,
            BinOp::Le,
            BinOp::Gt,
            BinOp::Ge,
            BinOp::And,
            BinOp::Or,
            BinOp::RefEq,
            BinOp::RefNe,
        ] {
            assert_eq!(op.arith_operator_name(), None);
        }
        assert_eq!(BinOp::from_arith_operator_name("nope"), None);
        assert_eq!(BinOp::from_arith_operator_name(""), None);
    }

    // ---- Modality accessors --------------------------------------------------

    #[test]
    fn modality_default_is_final() {
        let m = Modality::default();
        assert_eq!(m, Modality::Final);
        assert!(!m.is_abstract());
        assert!(!m.is_open());
        assert!(!m.is_sealed());
    }

    #[test]
    fn modality_open() {
        let m = Modality::Open;
        assert!(!m.is_abstract());
        assert!(m.is_open());
        assert!(!m.is_sealed());
    }

    #[test]
    fn modality_abstract() {
        let m = Modality::Abstract;
        assert!(m.is_abstract());
        assert!(!m.is_open());
        assert!(!m.is_sealed());
    }

    #[test]
    fn modality_sealed_is_abstract_and_open() {
        let m = Modality::Sealed;
        assert!(m.is_abstract());
        assert!(m.is_open());
        assert!(m.is_sealed());
    }

    // ---- ClassDecl accessors -------------------------------------------------

    #[test]
    fn class_decl_kind_accessors() {
        let c = class_decl("C", ClassKind::Class, Modality::Final);
        assert!(!c.is_interface());
        assert!(!c.is_object());
        assert!(!c.is_enum());
        assert!(!c.is_annotation());

        let i = class_decl("I", ClassKind::Interface, Modality::Abstract);
        assert!(i.is_interface());
        let o = class_decl("O", ClassKind::Object, Modality::Final);
        assert!(o.is_object());
        let e = class_decl("E", ClassKind::Enum, Modality::Final);
        assert!(e.is_enum());
        let a = class_decl("A", ClassKind::Annotation, Modality::Abstract);
        assert!(a.is_annotation());
    }

    #[test]
    fn class_decl_modality_accessors_delegate() {
        let sealed = class_decl("S", ClassKind::Class, Modality::Sealed);
        assert!(sealed.is_abstract());
        assert!(sealed.is_open());
        assert!(sealed.is_sealed());

        let plain = class_decl("P", ClassKind::Class, Modality::Final);
        assert!(!plain.is_abstract());
        assert!(!plain.is_open());
        assert!(!plain.is_sealed());
    }

    // ---- File arena add/get + spread ----------------------------------------

    #[test]
    fn file_arena_add_and_get() {
        let mut f = File::default();
        let a = f.add_expr(Expr::IntLit(1), sp());
        let b = f.add_expr(Expr::IntLit(2), sp());
        assert_eq!(a, ExprId(0));
        assert_eq!(b, ExprId(1));
        assert!(matches!(f.expr(a), Expr::IntLit(1)));
        assert!(matches!(f.expr(b), Expr::IntLit(2)));

        let s = f.add_stmt(Stmt::Expr(a), sp());
        assert_eq!(s, StmtId(0));
        assert!(matches!(f.stmt(s), Stmt::Expr(_)));

        let d = f.add_decl(Decl::Property(PropDecl {
            name: "x".to_string(),
            receiver: None,
            ty: None,
            is_var: false,
            init: Some(a),
            is_lateinit: false,
            getter: None,
            setter: None,
            is_const: false,
            is_abstract: false,
            delegate: None,
            span: sp(),
        }));
        assert_eq!(d, DeclId(0));
        assert!(matches!(f.decl(d), Decl::Property(_)));
        assert_eq!(f.expr_spans.len(), 2);
        assert_eq!(f.stmt_spans.len(), 1);
    }

    #[test]
    fn file_is_spread_arg() {
        let mut f = File::default();
        let a = f.add_expr(Expr::Name("arr".to_string()), sp());
        let b = f.add_expr(Expr::Name("plain".to_string()), sp());
        f.spread_arg_ids.insert(a.0);
        assert!(f.is_spread_arg(a));
        assert!(!f.is_spread_arg(b));
    }

    // ---- any_child_expr ------------------------------------------------------

    // Predicate that returns true only for the marked target id.
    fn hits(target: ExprId) -> impl FnMut(ExprId) -> bool {
        move |id| id == target
    }

    #[test]
    fn any_child_expr_leaf_is_false() {
        let mut f = File::default();
        let lit = f.add_expr(Expr::IntLit(7), sp());
        let name = f.add_expr(Expr::Name("z".to_string()), sp());
        let mut yes = |_: ExprId| true;
        let mut ys = |_: StmtId| true;
        assert!(!f.any_child_expr(lit, &mut yes, &mut ys));
        assert!(!f.any_child_expr(name, &mut yes, &mut ys));
    }

    #[test]
    fn any_child_expr_binary_both_branches() {
        let mut f = File::default();
        let l = f.add_expr(Expr::IntLit(1), sp());
        let r = f.add_expr(Expr::IntLit(2), sp());
        let bin = f.add_expr(
            Expr::Binary {
                op: BinOp::Add,
                lhs: l,
                rhs: r,
            },
            sp(),
        );
        let mut fs = |_: StmtId| false;
        // matches lhs
        let mut h = hits(l);
        assert!(f.any_child_expr(bin, &mut h, &mut fs));
        // matches rhs
        let mut h = hits(r);
        assert!(f.any_child_expr(bin, &mut h, &mut fs));
        // matches neither
        let mut none = |_: ExprId| false;
        assert!(!f.any_child_expr(bin, &mut none, &mut fs));
    }

    #[test]
    fn any_child_expr_unary_and_operand_group() {
        let mut f = File::default();
        let op = f.add_expr(Expr::BoolLit(true), sp());
        let un = f.add_expr(
            Expr::Unary {
                op: UnOp::Not,
                operand: op,
            },
            sp(),
        );
        let notnull = f.add_expr(Expr::NotNull { operand: op }, sp());
        let mut fs = |_: StmtId| false;
        let mut h = hits(op);
        assert!(f.any_child_expr(un, &mut h, &mut fs));
        let mut h = hits(op);
        assert!(f.any_child_expr(notnull, &mut h, &mut fs));
    }

    #[test]
    fn any_child_expr_call_callee_and_args() {
        let mut f = File::default();
        let callee = f.add_expr(Expr::Name("g".to_string()), sp());
        let arg0 = f.add_expr(Expr::IntLit(1), sp());
        let arg1 = f.add_expr(Expr::IntLit(2), sp());
        let call = f.add_expr(
            Expr::Call {
                callee,
                args: vec![arg0, arg1],
            },
            sp(),
        );
        let mut fs = |_: StmtId| false;
        let mut h = hits(callee);
        assert!(f.any_child_expr(call, &mut h, &mut fs));
        let mut h = hits(arg1);
        assert!(f.any_child_expr(call, &mut h, &mut fs));
        let mut none = |_: ExprId| false;
        assert!(!f.any_child_expr(call, &mut none, &mut fs));
    }

    #[test]
    fn any_child_expr_safecall_args_optional() {
        let mut f = File::default();
        let recv = f.add_expr(Expr::Name("r".to_string()), sp());
        let arg = f.add_expr(Expr::IntLit(9), sp());
        let with_args = f.add_expr(
            Expr::SafeCall {
                receiver: recv,
                name: "m".to_string(),
                args: Some(vec![arg]),
            },
            sp(),
        );
        let no_args = f.add_expr(
            Expr::SafeCall {
                receiver: recv,
                name: "p".to_string(),
                args: None,
            },
            sp(),
        );
        let mut fs = |_: StmtId| false;
        let mut h = hits(arg);
        assert!(f.any_child_expr(with_args, &mut h, &mut fs));
        // no_args: arg not reachable, receiver is
        let mut h = hits(arg);
        assert!(!f.any_child_expr(no_args, &mut h, &mut fs));
        let mut h = hits(recv);
        assert!(f.any_child_expr(no_args, &mut h, &mut fs));
    }

    #[test]
    fn any_child_expr_return_and_callableref_optionals() {
        let mut f = File::default();
        let v = f.add_expr(Expr::IntLit(3), sp());
        let ret_some = f.add_expr(
            Expr::Return {
                value: Some(v),
                label: None,
            },
            sp(),
        );
        let ret_none = f.add_expr(
            Expr::Return {
                value: None,
                label: Some("l".to_string()),
            },
            sp(),
        );
        let recv = f.add_expr(Expr::Name("r".to_string()), sp());
        let cref_recv = f.add_expr(
            Expr::CallableRef {
                receiver: Some(recv),
                name: "m".to_string(),
            },
            sp(),
        );
        let cref_none = f.add_expr(
            Expr::CallableRef {
                receiver: None,
                name: "top".to_string(),
            },
            sp(),
        );
        let mut fs = |_: StmtId| false;
        let mut yes = |_: ExprId| true;
        let mut h = hits(v);
        assert!(f.any_child_expr(ret_some, &mut h, &mut fs));
        assert!(!f.any_child_expr(ret_none, &mut yes, &mut fs));
        let mut h = hits(recv);
        assert!(f.any_child_expr(cref_recv, &mut h, &mut fs));
        assert!(!f.any_child_expr(cref_none, &mut yes, &mut fs));
    }

    #[test]
    fn any_child_expr_if_when_block_template_try() {
        let mut f = File::default();
        let cond = f.add_expr(Expr::BoolLit(true), sp());
        let then_b = f.add_expr(Expr::IntLit(1), sp());
        let else_b = f.add_expr(Expr::IntLit(2), sp());
        let iff = f.add_expr(
            Expr::If {
                cond,
                then_branch: then_b,
                else_branch: Some(else_b),
            },
            sp(),
        );
        let iff_noelse = f.add_expr(
            Expr::If {
                cond,
                then_branch: then_b,
                else_branch: None,
            },
            sp(),
        );

        let subj = f.add_expr(Expr::IntLit(5), sp());
        let armc = f.add_expr(Expr::IntLit(5), sp());
        let armb = f.add_expr(Expr::IntLit(6), sp());
        let when = f.add_expr(
            Expr::When {
                subject: Some(subj),
                arms: vec![WhenArm {
                    conditions: vec![armc],
                    body: armb,
                }],
            },
            sp(),
        );

        let trailing = f.add_expr(Expr::IntLit(8), sp());
        let inner_e = f.add_expr(Expr::IntLit(9), sp());
        let inner_s = f.add_stmt(Stmt::Expr(inner_e), sp());
        let block = f.add_expr(
            Expr::Block {
                stmts: vec![inner_s],
                trailing: Some(trailing),
            },
            sp(),
        );

        let tpart = f.add_expr(Expr::IntLit(11), sp());
        let tmpl = f.add_expr(
            Expr::Template(vec![
                TemplatePart::Str("a".to_string()),
                TemplatePart::Expr(tpart),
            ]),
            sp(),
        );

        let tbody = f.add_expr(Expr::IntLit(20), sp());
        let cbody = f.add_expr(Expr::IntLit(21), sp());
        let fbody = f.add_expr(Expr::IntLit(22), sp());
        let tri = f.add_expr(
            Expr::Try {
                body: tbody,
                catches: vec![CatchClause {
                    name: "e".to_string(),
                    ty: tref("E"),
                    body: cbody,
                }],
                finally: Some(fbody),
            },
            sp(),
        );

        let mut fs_false = |_: StmtId| false;

        let mut h = hits(else_b);
        assert!(f.any_child_expr(iff, &mut h, &mut fs_false));
        let mut h = hits(else_b);
        assert!(!f.any_child_expr(iff_noelse, &mut h, &mut fs_false));
        let mut h = hits(cond);
        assert!(f.any_child_expr(iff_noelse, &mut h, &mut fs_false));

        let mut h = hits(subj);
        assert!(f.any_child_expr(when, &mut h, &mut fs_false));
        let mut h = hits(armc);
        assert!(f.any_child_expr(when, &mut h, &mut fs_false));
        let mut h = hits(armb);
        assert!(f.any_child_expr(when, &mut h, &mut fs_false));

        // block: trailing via fe
        let mut h = hits(trailing);
        assert!(f.any_child_expr(block, &mut h, &mut fs_false));
        // block: stmt via fs
        let mut none = |_: ExprId| false;
        let mut fs_hit = |s: StmtId| s == inner_s;
        assert!(f.any_child_expr(block, &mut none, &mut fs_hit));

        let mut h = hits(tpart);
        assert!(f.any_child_expr(tmpl, &mut h, &mut fs_false));
        let mut none = |_: ExprId| false;
        assert!(!f.any_child_expr(tmpl, &mut none, &mut fs_false));

        let mut h = hits(cbody);
        assert!(f.any_child_expr(tri, &mut h, &mut fs_false));
        let mut h = hits(fbody);
        assert!(f.any_child_expr(tri, &mut h, &mut fs_false));
        let mut h = hits(tbody);
        assert!(f.any_child_expr(tri, &mut h, &mut fs_false));
    }

    #[test]
    fn any_child_expr_ranges_index_member() {
        let mut f = File::default();
        let lo = f.add_expr(Expr::IntLit(0), sp());
        let hi = f.add_expr(Expr::IntLit(9), sp());
        let rng = f.add_expr(
            Expr::RangeTo {
                lo,
                hi,
                kind: RangeKind::Through,
            },
            sp(),
        );
        let val = f.add_expr(Expr::IntLit(3), sp());
        let inr = f.add_expr(
            Expr::InRange {
                value: val,
                start: lo,
                end: hi,
                kind: RangeKind::Until,
                negated: false,
            },
            sp(),
        );
        let arr = f.add_expr(Expr::Name("a".to_string()), sp());
        let idx = f.add_expr(Expr::IntLit(1), sp());
        let index = f.add_expr(
            Expr::Index {
                array: arr,
                index: idx,
            },
            sp(),
        );
        let recv = f.add_expr(Expr::Name("o".to_string()), sp());
        let member = f.add_expr(
            Expr::Member {
                receiver: recv,
                name: "f".to_string(),
            },
            sp(),
        );
        let tgt = f.add_expr(Expr::Name("c".to_string()), sp());
        let incdec = f.add_expr(
            Expr::IncDec {
                target: tgt,
                dec: false,
                prefix: true,
            },
            sp(),
        );

        let mut fs = |_: StmtId| false;
        let mut h = hits(hi);
        assert!(f.any_child_expr(rng, &mut h, &mut fs));
        let mut h = hits(val);
        assert!(f.any_child_expr(inr, &mut h, &mut fs));
        let mut h = hits(idx);
        assert!(f.any_child_expr(index, &mut h, &mut fs));
        let mut h = hits(recv);
        assert!(f.any_child_expr(member, &mut h, &mut fs));
        let mut h = hits(tgt);
        assert!(f.any_child_expr(incdec, &mut h, &mut fs));
    }

    // ---- any_child_stmt ------------------------------------------------------

    #[test]
    fn any_child_stmt_leaf_false() {
        let mut f = File::default();
        let brk = f.add_stmt(Stmt::Break(None), sp());
        let cont = f.add_stmt(Stmt::Continue(Some("l".to_string())), sp());
        let ret0 = f.add_stmt(Stmt::Return(None, None), sp());
        let inc = f.add_stmt(
            Stmt::IncDec {
                name: "x".to_string(),
                dec: false,
            },
            sp(),
        );
        let li = f.add_stmt(
            Stmt::LocalLateinit {
                name: "y".to_string(),
                ty: tref("Int"),
            },
            sp(),
        );
        let mut yes = |_: ExprId| true;
        for s in [brk, cont, ret0, inc, li] {
            assert!(!f.any_child_stmt(s, &mut yes));
        }
    }

    #[test]
    fn any_child_stmt_single_init_group() {
        let mut f = File::default();
        let init = f.add_expr(Expr::IntLit(1), sp());
        let local = f.add_stmt(
            Stmt::Local {
                is_var: false,
                name: "a".to_string(),
                ty: None,
                init,
            },
            sp(),
        );
        let expr = f.add_stmt(Stmt::Expr(init), sp());
        let mut h = hits(init);
        assert!(f.any_child_stmt(local, &mut h));
        let mut h = hits(init);
        assert!(f.any_child_stmt(expr, &mut h));
        let mut none = |_: ExprId| false;
        assert!(!f.any_child_stmt(local, &mut none));
    }

    #[test]
    fn any_child_stmt_multi_child() {
        let mut f = File::default();
        let recv = f.add_expr(Expr::Name("o".to_string()), sp());
        let value = f.add_expr(Expr::IntLit(2), sp());
        let am = f.add_stmt(
            Stmt::AssignMember {
                receiver: recv,
                name: "p".to_string(),
                value,
            },
            sp(),
        );
        let arr = f.add_expr(Expr::Name("a".to_string()), sp());
        let idx = f.add_expr(Expr::IntLit(0), sp());
        let ai = f.add_stmt(
            Stmt::AssignIndex {
                array: arr,
                index: idx,
                value,
            },
            sp(),
        );
        let cond = f.add_expr(Expr::BoolLit(true), sp());
        let body = f.add_expr(Expr::IntLit(5), sp());
        let whl = f.add_stmt(
            Stmt::While {
                cond,
                body,
                label: None,
            },
            sp(),
        );
        let iterable = f.add_expr(Expr::Name("xs".to_string()), sp());
        let fe = f.add_stmt(
            Stmt::ForEach {
                name: "e".to_string(),
                iterable,
                body,
                label: None,
            },
            sp(),
        );
        let rstart = f.add_expr(Expr::IntLit(0), sp());
        let rend = f.add_expr(Expr::IntLit(9), sp());
        let forr = f.add_stmt(
            Stmt::For {
                name: "i".to_string(),
                range: ForRange {
                    start: rstart,
                    end: rend,
                    kind: RangeKind::Through,
                },
                body,
                label: None,
            },
            sp(),
        );

        let mut h = hits(value);
        assert!(f.any_child_stmt(am, &mut h));
        let mut h = hits(recv);
        assert!(f.any_child_stmt(am, &mut h));
        let mut h = hits(idx);
        assert!(f.any_child_stmt(ai, &mut h));
        let mut h = hits(cond);
        assert!(f.any_child_stmt(whl, &mut h));
        let mut h = hits(iterable);
        assert!(f.any_child_stmt(fe, &mut h));
        let mut h = hits(rend);
        assert!(f.any_child_stmt(forr, &mut h));
    }

    #[test]
    fn any_child_stmt_localfun_and_localclass() {
        let mut f = File::default();
        let body = f.add_expr(Expr::IntLit(1), sp());
        let lf = f.add_stmt(
            Stmt::LocalFun(FunDecl {
                name: "g".to_string(),
                receiver: None,
                params: Vec::new(),
                ret: None,
                body: FunBody::Expr(body),
                type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                non_null_type_params: std::collections::HashSet::new(),
                reified_type_params: std::collections::HashSet::new(),
                span: sp(),
                is_inline: false,
                is_final: false,
                is_abstract: false,
                is_private: false,
                is_suspend: false,
                is_tailrec: false,
                annotations: Vec::new(),
            }),
            sp(),
        );
        let lc = f.add_stmt(
            Stmt::LocalClass(class_decl("L", ClassKind::Class, Modality::Final)),
            sp(),
        );
        let mut h = hits(body);
        assert!(f.any_child_stmt(lf, &mut h));
        let mut yes = |_: ExprId| true;
        assert!(!f.any_child_stmt(lc, &mut yes));
    }

    // ---- debug_tree / write_* ------------------------------------------------

    #[test]
    fn debug_tree_property_and_fun() {
        let mut f = File::default();
        let init = f.add_expr(Expr::IntLit(42), sp());
        let d = f.add_decl(Decl::Property(PropDecl {
            name: "x".to_string(),
            receiver: None,
            ty: Some(tref("Int")),
            is_var: true,
            init: Some(init),
            is_lateinit: false,
            getter: None,
            setter: None,
            is_const: false,
            is_abstract: false,
            delegate: None,
            span: sp(),
        }));
        f.decls.push(d);
        let out = f.debug_tree();
        assert!(out.contains("(var x :Int 42)"), "got: {out}");

        let mut f = File::default();
        let body = f.add_expr(Expr::Name("y".to_string()), sp());
        let d = f.add_decl(Decl::Fun(FunDecl {
            name: "foo".to_string(),
            receiver: None,
            params: vec![Param {
                name: "a".to_string(),
                ty: tref("Int"),
                is_vararg: false,
                default: None,
                annotations: Vec::new(),
                annotation_args: Vec::new(),
            }],
            ret: Some(tref("Int")),
            body: FunBody::Expr(body),
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            non_null_type_params: std::collections::HashSet::new(),
            reified_type_params: std::collections::HashSet::new(),
            span: sp(),
            is_inline: false,
            is_final: false,
            is_abstract: false,
            is_private: false,
            is_suspend: false,
            is_tailrec: false,
            annotations: Vec::new(),
        }));
        f.decls.push(d);
        let out = f.debug_tree();
        assert!(out.contains("(fun foo (param a Int) :Int y)"), "got: {out}");
    }

    #[test]
    fn debug_tree_class_interface_enum_object() {
        // class with a prop + a method
        let mut f = File::default();
        let mut c = class_decl("C", ClassKind::Class, Modality::Final);
        c.props.push(PropParam {
            name: "n".to_string(),
            ty: tref("Int"),
            is_var: false,
            is_property: true,
            default: None,
            annotations: Vec::new(),
            annotation_args: Vec::new(),
        });
        c.methods.push(FunDecl {
            name: "m".to_string(),
            receiver: None,
            params: Vec::new(),
            ret: Some(tref("Unit")),
            body: FunBody::None,
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            non_null_type_params: std::collections::HashSet::new(),
            reified_type_params: std::collections::HashSet::new(),
            span: sp(),
            is_inline: false,
            is_final: false,
            is_abstract: false,
            is_private: false,
            is_suspend: false,
            is_tailrec: false,
            annotations: Vec::new(),
        });
        let d = f.add_decl(Decl::Class(c));
        f.decls.push(d);
        let out = f.debug_tree();
        assert!(
            out.contains("(class C (val n Int) (method m :Unit))"),
            "got: {out}"
        );

        // interface
        let mut f = File::default();
        let mut i = class_decl("I", ClassKind::Interface, Modality::Abstract);
        i.methods.push(FunDecl {
            name: "am".to_string(),
            receiver: None,
            params: Vec::new(),
            ret: None,
            body: FunBody::None,
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            non_null_type_params: std::collections::HashSet::new(),
            reified_type_params: std::collections::HashSet::new(),
            span: sp(),
            is_inline: false,
            is_final: false,
            is_abstract: false,
            is_private: false,
            is_suspend: false,
            is_tailrec: false,
            annotations: Vec::new(),
        });
        let d = f.add_decl(Decl::Class(i));
        f.decls.push(d);
        assert!(f.debug_tree().contains("(interface I (absfun am))"));

        // enum
        let mut f = File::default();
        let mut e = class_decl("E", ClassKind::Enum, Modality::Final);
        e.enum_entries.push(AstEnumEntry {
            name: "A".to_string(),
            args: Vec::new(),
            methods: Vec::new(),
            props: Vec::new(),
        });
        let d = f.add_decl(Decl::Class(e));
        f.decls.push(d);
        assert!(f.debug_tree().contains("(enum E A)"));

        // object
        let mut f = File::default();
        let o = class_decl("O", ClassKind::Object, Modality::Final);
        let d = f.add_decl(Decl::Class(o));
        f.decls.push(d);
        assert!(f.debug_tree().contains("(object O)"));
    }

    #[test]
    fn write_expr_covers_many_variants() {
        let mut f = File::default();
        let lit_i = f.add_expr(Expr::IntLit(1), sp());
        let lit_l = f.add_expr(Expr::LongLit(2), sp());
        let lit_u = f.add_expr(Expr::UIntLit(3), sp());
        let lit_ul = f.add_expr(Expr::ULongLit(4), sp());
        let lit_d = f.add_expr(Expr::DoubleLit(1.5), sp());
        let lit_f = f.add_expr(Expr::FloatLit(2.5), sp());
        let lit_b = f.add_expr(Expr::BoolLit(false), sp());
        let lit_s = f.add_expr(Expr::StringLit("hi".to_string()), sp());
        let lit_c = f.add_expr(Expr::CharLit('q'), sp());
        let lit_n = f.add_expr(Expr::NullLit, sp());

        // Build a big binary/template/when/incdec tree and render via a fun body.
        let a = f.add_expr(Expr::Name("a".to_string()), sp());
        let b = f.add_expr(Expr::Name("b".to_string()), sp());
        let elvis = f.add_expr(Expr::Elvis { lhs: a, rhs: b }, sp());
        let throw = f.add_expr(Expr::Throw { operand: b }, sp());
        let notnull = f.add_expr(Expr::NotNull { operand: a }, sp());
        let ret = f.add_expr(
            Expr::Return {
                value: Some(a),
                label: Some("L".to_string()),
            },
            sp(),
        );
        let lam = f.add_expr(
            Expr::Lambda {
                params: Vec::new(),
                body: a,
            },
            sp(),
        );
        let lam2 = f.add_expr(
            Expr::Lambda {
                params: vec!["p".to_string()],
                body: a,
            },
            sp(),
        );
        let idx = f.add_expr(Expr::Index { array: a, index: b }, sp());
        let is_e = f.add_expr(
            Expr::Is {
                operand: a,
                ty: tref("Int"),
                negated: true,
            },
            sp(),
        );
        let as_e = f.add_expr(
            Expr::As {
                operand: a,
                ty: tref("Int"),
                nullable: true,
            },
            sp(),
        );
        let inr = f.add_expr(
            Expr::InRange {
                value: a,
                start: lit_i,
                end: b,
                kind: RangeKind::DownTo,
                negated: true,
            },
            sp(),
        );
        let rng = f.add_expr(
            Expr::RangeTo {
                lo: lit_i,
                hi: b,
                kind: RangeKind::Until,
            },
            sp(),
        );
        let incdec = f.add_expr(
            Expr::IncDec {
                target: a,
                dec: true,
                prefix: false,
            },
            sp(),
        );
        let safe = f.add_expr(
            Expr::SafeCall {
                receiver: a,
                name: "m".to_string(),
                args: Some(vec![b]),
            },
            sp(),
        );
        let tmpl = f.add_expr(
            Expr::Template(vec![
                TemplatePart::Str("x".to_string()),
                TemplatePart::Expr(a),
            ]),
            sp(),
        );
        let unary = f.add_expr(
            Expr::Unary {
                op: UnOp::Neg,
                operand: a,
            },
            sp(),
        );
        let bin = f.add_expr(
            Expr::Binary {
                op: BinOp::Lt,
                lhs: a,
                rhs: b,
            },
            sp(),
        );
        let member = f.add_expr(
            Expr::Member {
                receiver: a,
                name: "f".to_string(),
            },
            sp(),
        );
        let call = f.add_expr(
            Expr::Call {
                callee: a,
                args: vec![b],
            },
            sp(),
        );
        let iff = f.add_expr(
            Expr::If {
                cond: lit_b,
                then_branch: a,
                else_branch: Some(b),
            },
            sp(),
        );
        let when = f.add_expr(
            Expr::When {
                subject: Some(a),
                arms: vec![
                    WhenArm {
                        conditions: vec![lit_i],
                        body: b,
                    },
                    WhenArm {
                        conditions: Vec::new(),
                        body: a,
                    },
                ],
            },
            sp(),
        );
        let cref = f.add_expr(
            Expr::CallableRef {
                receiver: Some(a),
                name: "r".to_string(),
            },
            sp(),
        );
        let inner_e = f.add_expr(Expr::IntLit(0), sp());
        let inner_s = f.add_stmt(Stmt::Expr(inner_e), sp());
        let block = f.add_expr(
            Expr::Block {
                stmts: vec![inner_s],
                trailing: Some(a),
            },
            sp(),
        );

        // Render each via a one-off fun body, checking key fragments.
        let cases: &[(ExprId, &str)] = &[
            (lit_i, "1"),
            (lit_l, "2L"),
            (lit_u, "3u"),
            (lit_ul, "4uL"),
            (lit_d, "1.5d"),
            (lit_f, "2.5f"),
            (lit_b, "false"),
            (lit_s, "\"hi\""),
            (lit_c, "'q'"),
            (lit_n, "null"),
            (elvis, "(?: a b)"),
            (throw, "(throw b)"),
            (notnull, "(!! a)"),
            (ret, "(return@L a)"),
            (lam, "(lambda it a)"),
            (lam2, "(lambda p a)"),
            (idx, "(index a b)"),
            (is_e, "(!is a Int)"),
            (as_e, "(as? a Int)"),
            (inr, "(!in a downTo 1 b)"),
            (rng, "(..< 1 b)"),
            (incdec, "(post-- a)"),
            (safe, "(?. a m b)"),
            (tmpl, "(template \"x\" a)"),
            (unary, "(neg a)"),
            (bin, "(< a b)"),
            (member, "(. a f)"),
            (call, "(call a b)"),
            (iff, "(if false a b)"),
            (when, "(when a (arm 1 => b) (arm else => a))"),
            (cref, "a::r"),
            (block, "(block 0 =>a)"),
        ];
        for (e, frag) in cases {
            let d = f.add_decl(Decl::Fun(FunDecl {
                name: "w".to_string(),
                receiver: None,
                params: Vec::new(),
                ret: None,
                body: FunBody::Expr(*e),
                type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                non_null_type_params: std::collections::HashSet::new(),
                reified_type_params: std::collections::HashSet::new(),
                span: sp(),
                is_inline: false,
                is_final: false,
                is_abstract: false,
                is_private: false,
                is_suspend: false,
                is_tailrec: false,
                annotations: Vec::new(),
            }));
            f.decls.clear();
            f.decls.push(d);
            let out = f.debug_tree();
            assert!(out.contains(frag), "expected {frag:?} in {out:?}");
        }
    }

    #[test]
    fn write_stmt_covers_many_variants() {
        let mut f = File::default();
        let e = f.add_expr(Expr::Name("v".to_string()), sp());
        let recv = f.add_expr(Expr::Name("o".to_string()), sp());
        let idx = f.add_expr(Expr::IntLit(0), sp());

        let local = f.add_stmt(
            Stmt::Local {
                is_var: true,
                name: "a".to_string(),
                ty: None,
                init: e,
            },
            sp(),
        );
        let lateinit = f.add_stmt(
            Stmt::LocalLateinit {
                name: "b".to_string(),
                ty: tref("Int"),
            },
            sp(),
        );
        let deleg = f.add_stmt(
            Stmt::LocalDelegate {
                is_var: false,
                name: "c".to_string(),
                ty: None,
                delegate: e,
            },
            sp(),
        );
        let destr = f.add_stmt(
            Stmt::Destructure {
                entries: vec![("x".to_string(), false), ("y".to_string(), true)],
                init: e,
            },
            sp(),
        );
        let assign = f.add_stmt(
            Stmt::Assign {
                name: "a".to_string(),
                value: e,
            },
            sp(),
        );
        let inc = f.add_stmt(
            Stmt::IncDec {
                name: "a".to_string(),
                dec: true,
            },
            sp(),
        );
        let am = f.add_stmt(
            Stmt::AssignMember {
                receiver: recv,
                name: "p".to_string(),
                value: e,
            },
            sp(),
        );
        let ai = f.add_stmt(
            Stmt::AssignIndex {
                array: recv,
                index: idx,
                value: e,
            },
            sp(),
        );
        let brk = f.add_stmt(Stmt::Break(Some("outer".to_string())), sp());
        let cont = f.add_stmt(Stmt::Continue(None), sp());
        let ret = f.add_stmt(Stmt::Return(Some(e), Some("L".to_string())), sp());
        let whl = f.add_stmt(
            Stmt::While {
                cond: e,
                body: e,
                label: None,
            },
            sp(),
        );
        let dow = f.add_stmt(
            Stmt::DoWhile {
                body: e,
                cond: e,
                label: None,
            },
            sp(),
        );
        let rstart = f.add_expr(Expr::IntLit(0), sp());
        let rend = f.add_expr(Expr::IntLit(9), sp());
        let forr = f.add_stmt(
            Stmt::For {
                name: "i".to_string(),
                range: ForRange {
                    start: rstart,
                    end: rend,
                    kind: RangeKind::Until,
                },
                body: e,
                label: None,
            },
            sp(),
        );
        let fe = f.add_stmt(
            Stmt::ForEach {
                name: "z".to_string(),
                iterable: e,
                body: e,
                label: None,
            },
            sp(),
        );
        let lc = f.add_stmt(
            Stmt::LocalClass(class_decl("Loc", ClassKind::Class, Modality::Final)),
            sp(),
        );

        let cases: &[(StmtId, &str)] = &[
            (local, "(var a v)"),
            (lateinit, "(lateinit var b)"),
            (deleg, "(val c by v)"),
            (destr, "(destructure (x y) v)"),
            (assign, "(set a v)"),
            (inc, "(dec a)"),
            (am, "(set-member o p v)"),
            (ai, "(set-index o 0 v)"),
            (brk, "(break@outer)"),
            (cont, "(continue)"),
            (ret, "(return@L v)"),
            (whl, "(while v v)"),
            (dow, "(do v while v)"),
            (forr, "(for i (0 until 9) v)"),
            (fe, "(for-each z v v)"),
            (lc, "(local-class Loc)"),
        ];
        for (s, frag) in cases {
            let block = f.add_expr(
                Expr::Block {
                    stmts: vec![*s],
                    trailing: None,
                },
                sp(),
            );
            let d = f.add_decl(Decl::Fun(FunDecl {
                name: "w".to_string(),
                receiver: None,
                params: Vec::new(),
                ret: None,
                body: FunBody::Block(block),
                type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                non_null_type_params: std::collections::HashSet::new(),
                reified_type_params: std::collections::HashSet::new(),
                span: sp(),
                is_inline: false,
                is_final: false,
                is_abstract: false,
                is_private: false,
                is_suspend: false,
                is_tailrec: false,
                annotations: Vec::new(),
            }));
            f.decls.clear();
            f.decls.push(d);
            let out = f.debug_tree();
            assert!(out.contains(frag), "expected {frag:?} in {out:?}");
        }
    }

    // A local-fun statement renders by name only.
    #[test]
    fn write_stmt_local_fun() {
        let mut f = File::default();
        let body = f.add_expr(Expr::IntLit(1), sp());
        let lf = f.add_stmt(
            Stmt::LocalFun(FunDecl {
                name: "inner".to_string(),
                receiver: None,
                params: Vec::new(),
                ret: None,
                body: FunBody::Expr(body),
                type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                non_null_type_params: std::collections::HashSet::new(),
                reified_type_params: std::collections::HashSet::new(),
                span: sp(),
                is_inline: false,
                is_final: false,
                is_abstract: false,
                is_private: false,
                is_suspend: false,
                is_tailrec: false,
                annotations: Vec::new(),
            }),
            sp(),
        );
        let block = f.add_expr(
            Expr::Block {
                stmts: vec![lf],
                trailing: None,
            },
            sp(),
        );
        let d = f.add_decl(Decl::Fun(FunDecl {
            name: "outer".to_string(),
            receiver: None,
            params: Vec::new(),
            ret: None,
            body: FunBody::Block(block),
            type_params: Vec::new(),
            type_param_bounds: Vec::new(),
            non_null_type_params: std::collections::HashSet::new(),
            reified_type_params: std::collections::HashSet::new(),
            span: sp(),
            is_inline: false,
            is_final: false,
            is_abstract: false,
            is_private: false,
            is_suspend: false,
            is_tailrec: false,
            annotations: Vec::new(),
        }));
        f.decls.push(d);
        assert!(f.debug_tree().contains("(local-fun inner)"));
    }
}
