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
//! terms (`Ty`), never JVM descriptors.
//!
//! Storage follows krusty's index-based invariant: nodes live in parallel `Vec` arenas keyed by
//! `u32` ids (no `Box`/`Rc` graphs; bulk-freeable). Lowering (`ast → ir`) and the JVM backend
//! consuming IR are the next phases; today this module defines the node set + a builder + a printer.

use crate::libraries::InlineKind;
use crate::types::Ty;

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
    /// The `$default` synthetic of a same-file top-level function/extension (`FunId`) — emitted as
    /// `invokestatic <facade>.<name>$default(realparams, int mask, Object marker)ret`. Like `Local` the
    /// facade is resolved at emit (`self.facade`); the descriptor appends the trailing `I` mask +
    /// `Object` marker to the real function's parameters. Used when a call omits a (possibly non-const)
    /// defaulted argument, mirroring kotlinc's default-argument ABI.
    LocalDefault(FunId),
    External(String),
    /// A top-level function defined in ANOTHER source file of the same multi-file compilation —
    /// `invokestatic <facade>.<name>(params)ret`. Carries the signature as backend-agnostic `Ty`s
    /// (the JVM backend builds the descriptor), so `ir_lower` needn't know JVM descriptors. Distinct
    /// from `Local` (same IrFile, by index) and `Static` (a resolved classpath/library method).
    CrossFile {
        facade: String,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// An instance method (or property accessor) of a class defined in ANOTHER file of the same
    /// compilation — `invokevirtual`/`invokeinterface owner.name(params)ret` on the `dispatch_receiver`.
    /// Like `Virtual` but carries `Ty`s (the JVM backend builds the descriptor), so `ir_lower` needs
    /// no JVM descriptor for a sibling-file user class (resolved from its `ClassSig`).
    CrossFileVirtual {
        owner: String,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        interface: bool,
    },
    /// A resolved classpath static method — `invokestatic owner.name:descriptor`. Used for stdlib
    /// extension/top-level functions resolved from the classpath (`StringsKt.repeat`, `RangesKt.until`),
    /// carrying the exact JVM descriptor so no name is hardcoded in the backend.
    /// `inline` carries the callee's inline-ness in one field (was `inline` + `must_inline`):
    /// [`InlineKind::CanInline`] => a Kotlin `inline` function whose compiled body the JVM backend may
    /// splice here instead of emitting the `invokestatic`; [`InlineKind::MustInline`] => a NON-PUBLIC
    /// `@InlineOnly` callee (`require`/`check`/`error`) with no legal `invokestatic` fallback, so the
    /// backend MUST splice the body (a body it can't splice — e.g. branchy on a non-empty operand stack —
    /// skips the whole file, never miscompiled).
    Static {
        owner: String,
        name: String,
        descriptor: String,
        inline: InlineKind,
    },
    /// A resolved classpath *instance* method — `invokevirtual`/`invokeinterface owner.name:descriptor`
    /// on the `dispatch_receiver`. `owner` is the receiver's static type; `interface` ⇒ `invokeinterface`.
    Virtual {
        owner: String,
        name: String,
        descriptor: String,
        interface: bool,
    },
    /// A non-virtual instance call — `invokespecial owner.name:descriptor` on the `dispatch_receiver`.
    /// Used for `super.method(…)`, which dispatches to the named base-class method directly (skipping the
    /// receiver's override). `owner` is the base class declaring the method.
    Special {
        owner: String,
        name: String,
        descriptor: String,
        /// `owner` is an INTERFACE (a diamond `super.f()` dispatched to a superinterface's DEFAULT method):
        /// the method reference must be an `InterfaceMethodref` and the call an `invokespecial` on it.
        interface: bool,
    },
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
    /// A class-literal constant — `ldc class <internal>` (a `java.lang.Class`). Used e.g. for the
    /// `PropertyReference0Impl(Class, …)` argument in delegated-property setup. `internal` is the JVM
    /// internal name (mapped through the backend's kotlin→java table at emit).
    ClassConst {
        internal: String,
    },
    /// Read a value parameter / variable by its declaration index.
    GetValue(u32),
    /// Assign to a variable (`IrSetValue`).
    SetValue {
        var: u32,
        value: ExprId,
    },
    /// A call to a function/constructor/operator/stdlib intrinsic (`IrCall`). The `callee` is a
    /// resolved [`Callee`]: a local function, or an intrinsic identified by Kotlin FqName that each
    /// backend maps to its platform (`kotlin/String.plus`, `kotlin/io/println`, …). This single node
    /// expresses every call — there is no dedicated node per stdlib operation.
    Call {
        callee: Callee,
        dispatch_receiver: Option<ExprId>,
        args: Vec<ExprId>,
    },
    /// A placeholder a compiler-extension plugin must specialize before emit — core lowering produces
    /// it generically (no plugin-specific ABI), and the plugin rewrites this arena slot into concrete
    /// IR in its body phase. Currently the only producer is the reified kotlinx.serialization round-trip
    /// (`fmt.encodeToString(x)` / `fmt.decodeFromString<C>(s)`): core detects the call STRUCTURALLY (a
    /// `StringFormat` receiver + a `@Serializable` type) and records the resolved facts here; the
    /// serialization plugin owns the `StringFormat` member descriptors (so a new serialization runtime
    /// doesn't require recompiling core). `exprs` are already-lowered operands, `data` carries resolved
    /// names; the meaning of both is private to the named plugin. A node that survives to emit is
    /// declined by `jvm_can_emit` (never miscompiled).
    PluginPlaceholder {
        /// Which plugin specializes this node (e.g. `"serialization"`).
        plugin: &'static str,
        /// The plugin-specific operation (e.g. `"encodeToString"` / `"decodeFromString"`).
        kind: &'static str,
        /// Already-lowered operand expressions, in a plugin-defined order.
        exprs: Vec<ExprId>,
        /// Resolved names the plugin needs (e.g. the format internal name, the `@Serializable` class).
        data: Vec<String>,
    },
    /// `IrReturn` from the enclosing function.
    Return(Option<ExprId>),
    /// `IrBlock` — a sequence of statements; value is the last expression (or Unit).
    Block {
        stmts: Vec<ExprId>,
        value: Option<ExprId>,
    },
    /// `IrWhen` — branches of (condition → result); the AST `if`/`when` lower here. `else` is the
    /// branch with a `None` condition.
    When {
        branches: Vec<(Option<ExprId>, ExprId)>,
    },
    /// `IrTypeOperatorCall` — `is`/`!is`/`as`/`as?`/implicit casts/coercions.
    TypeOp {
        op: IrTypeOp,
        arg: ExprId,
        type_operand: Ty,
    },
    /// `IrWhile` loop. `update` (if present) runs after `body` each iteration, at the `continue`
    /// target — it carries a `for`-loop's increment so `continue` advances the loop rather than
    /// skipping it. A plain `while` has `update: None` (then `continue` re-tests `cond`). `post_test`
    /// ⇒ a `do…while` (the body runs once before `cond` is first tested).
    While {
        cond: ExprId,
        body: ExprId,
        update: Option<ExprId>,
        post_test: bool,
        label: Option<String>,
    },
    /// `break` — exit the innermost enclosing loop, or the loop carrying `label` (`break@outer`).
    Break {
        label: Option<String>,
    },
    /// `continue` — jump to the innermost enclosing loop's `update`/condition (or the labeled loop's).
    Continue {
        label: Option<String>,
    },
    /// A local variable declaration (`IrVariable`), value optional (`lateinit`).
    Variable {
        index: u32,
        ty: Ty,
        init: Option<ExprId>,
    },
    /// A built-in primitive binary operator (`+`/`-`/`<`/`==`/…) on numeric/boolean operands. One
    /// parameterized node (not one-per-intrinsic): Kotlin IR models these as `IrCall` to the
    /// operator function, but the built-in numeric/boolean ops are universal across backends, so a
    /// single node lets each emit the native instruction (JVM `iadd`, JS `+`). Every *other*
    /// operator/stdlib operation — `String.plus`, `toString`, `println`, collections — is an
    /// ordinary `Call` to a `Callee::External` symbol the backend resolves; there is no per-op node.
    PrimitiveBinOp {
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    /// A Kotlin string template `"a${x}b"` as an ordered list of parts (string constants + interpolated
    /// values, with empty constant chunks dropped). The JVM backend emits it as kotlinc does: a single
    /// part → `String.valueOf(part)`; multiple parts → one `StringBuilder` with a typed `append` per part
    /// and a final `toString()` (vs the old `String.plus` chain, which made one StringBuilder per `+`).
    StringConcat(Vec<ExprId>),
    /// Read an instance field (`IrGetField`): `receiver.<fields[index]>` of class `class`.
    GetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
    },
    /// Write an instance field (`IrSetField`): `receiver.<fields[index]> = value` (statement).
    SetField {
        receiver: ExprId,
        class: ClassId,
        index: u32,
        value: ExprId,
    },
    /// Read a top-level (module) property — `statics[index]`, a static field on the file facade.
    GetStatic(u32),
    /// Write a top-level (module) property — `statics[index] = value` (statement).
    SetStatic {
        index: u32,
        value: ExprId,
    },
    /// Construct an instance (`IrConstructorCall`) of `class` with constructor `args` (in field order).
    /// `ctor_params` is `None` for the primary constructor (the descriptor covers the leading
    /// parameter fields); `Some(types)` selects a secondary constructor with that parameter list.
    New {
        class: ClassId,
        args: Vec<ExprId>,
        ctor_params: Option<Vec<Ty>>,
    },
    /// A virtual call to a class instance method `methods[index]` of `class` on `receiver`. `args[i] =
    /// None` means parameter `i` is omitted and takes its default (`p.copy(y=5)`, `f(a)` of `f(a, b=…)`);
    /// the meaning is backend-agnostic — the JVM realizes omitted args via the `$default` stub + mask,
    /// another backend may fill them inline. All-`Some` is an ordinary full call.
    MethodCall {
        class: ClassId,
        index: u32,
        receiver: ExprId,
        args: Vec<Option<ExprId>>,
    },
    /// Read an enum entry constant: `Enum.ENTRY` — `getstatic <class>.<entry>:L<class>;`.
    EnumEntry {
        class: ClassId,
        index: u32,
    },
    /// Read a static field holding a singleton instance (Kotlin IR's `IrGetObjectValue`):
    /// `getstatic <owner>.<field>:L<ty>;`. An `object`'s `INSTANCE` (`owner == ty`), or a
    /// `companion`'s `Companion` field on the outer class (`owner` = outer, `ty` = companion).
    StaticInstance {
        owner: ClassId,
        ty: ClassId,
        field: &'static str,
    },
    /// Read a static field of a CLASSPATH class by name — `getstatic owner.name:descriptor`. Used for a
    /// classpath `object` referenced as a value (`EmptyCoroutineContext` → `getstatic kotlin/coroutines/
    /// EmptyCoroutineContext.INSTANCE:Lkotlin/coroutines/EmptyCoroutineContext;`). Unlike `StaticInstance`
    /// (a user `ClassId`) and `GetStatic` (a facade statics index), this names an external owner directly.
    ExternalStaticField {
        owner: String,
        name: String,
        descriptor: String,
    },
    /// Call a static method of a class (`Enum.values()`, `Enum.valueOf(s)`).
    EnumValues {
        class: ClassId,
    },
    EnumValueOf {
        class: ClassId,
        arg: ExprId,
    },
    /// A lambda literal — emitted as `invokedynamic` + `LambdaMetafactory`. `impl_fn` is the
    /// synthesized static method holding the body; `captures` are the free-variable values bound into
    /// the call site (empty = non-capturing). `sam` is `None` for a plain Kotlin lambda (target
    /// `kotlin/jvm/functions/Function{arity}.invoke`), or `Some((interface, method))` for a SAM
    /// conversion to a user functional interface (`Pred { … }` → `Pred.test`).
    /// `inline_body` is the lambda's *value-producing* body form (no synthetic `return`), emitted
    /// directly when the lambda is inlined into a stdlib `inline fun` splice — so a user `return` in the
    /// lambda becomes a real return from the *enclosing* method (correct non-local return). `None` for a
    /// callable reference (`::foo`), which has no inlinable body.
    Lambda {
        impl_fn: u32,
        arity: u8,
        captures: Vec<ExprId>,
        sam: Option<(String, String)>,
        inline_body: Option<ExprId>,
    },
    /// The `kotlin.Unit` singleton value (`IrGetObjectValue` of `Unit`). On the JVM, `getstatic
    /// kotlin/Unit.INSTANCE:Lkotlin/Unit;` — what a `Unit`-returning lambda body yields so its
    /// `FunctionN.invoke` returns an `Object`. Another backend realizes the unit value differently.
    UnitInstance,
    /// The enclosing suspend function's own `Continuation` — the receiver bound to the lambda parameter
    /// of `suspendCoroutineUninterceptedOrReturn { c -> … }`. A placeholder emitted by `ir_lower` that the
    /// CPS pass (`jvm/suspend.rs`) rewrites to the real continuation value (`GetValue(<cont slot>)`) once
    /// the trailing `Continuation` parameter exists. It must never survive to the emitter.
    CurrentContinuation,
    /// Invoke a function value (`f(args)` where `f: (A,…) -> R`) via the `FunctionN.invoke` interface
    /// method. Arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`.
    InvokeFunction {
        func: ExprId,
        args: Vec<ExprId>,
        ret: Ty,
    },
    /// The not-null assertion `operand!!` — yields `operand`, throwing if it is null. On the JVM this
    /// is `kotlin/jvm/internal/Intrinsics.checkNotNull` applied to a duplicate of the value.
    NotNullAssert {
        operand: ExprId,
    },
    /// A `lateinit` read: yields `operand`, throwing `UninitializedPropertyAccessException(name)` if it
    /// is still null. Emitted as `<operand>; dup; ifnonnull L; ldc name;
    /// invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:` — the same guard the
    /// member-field lateinit read uses, here for a `lateinit var` LOCAL slot read.
    LateinitCheck {
        operand: ExprId,
        name: String,
    },
    /// Construct an instance of a classpath (non-IR) class — `RuntimeException("x")`, `StringBuilder()`.
    /// `internal` is the JVM internal name, `ctor_desc` the `(…)V` constructor descriptor.
    NewExternal {
        internal: String,
        ctor_desc: String,
        args: Vec<ExprId>,
    },
    /// Read a static field holding a singleton on a class defined OUTSIDE this compilation (a classpath
    /// class with no `IrClass`): `getstatic <owner>.<field>:L<ty>;`. Like `StaticInstance` but the owner
    /// and field type are given by internal name directly (e.g. a kotlinx builtin serializer's
    /// `kotlinx/serialization/internal/StringSerializer.INSTANCE`), not resolved through `ir.classes`.
    ExternalStaticInstance {
        owner: String,
        ty: String,
        field: String,
    },
    /// Construct a class defined in ANOTHER file of the same compilation — `new internal; dup; <args>;
    /// invokespecial internal.<init>(params)V`. Like `NewExternal` but carries the ctor parameter types
    /// as `Ty`s (the JVM backend builds the descriptor) since it's a sibling-file user class, not a
    /// classpath one with a library-provided descriptor.
    NewCrossFile {
        internal: String,
        params: Vec<Ty>,
        args: Vec<ExprId>,
    },
    /// A `kotlin/jvm/internal/Ref$XxxRef` holder boxing a mutable local that a closure captures: a
    /// new `Ref$IntRef`/`Ref$ObjectRef`/… whose `element` field is initialized to `init`. `elem` is
    /// the boxed value's type (selects the `Ref` subclass + the `element` field descriptor). Evaluates
    /// to the holder, so it's the initializer of the local that holds the box.
    RefNew {
        elem: Ty,
        init: ExprId,
    },
    /// Read a boxed mutable local: `holder.element` (`getfield Ref$XxxRef.element`).
    RefGet {
        holder: ExprId,
        elem: Ty,
    },
    /// Write a boxed mutable local: `holder.element = value` (`putfield`), evaluating to `value`.
    RefSet {
        holder: ExprId,
        elem: Ty,
        value: ExprId,
    },
    /// `throw operand` — throws the (Throwable) value; control never falls through (`Nothing`).
    Throw {
        operand: ExprId,
    },
    /// A `vararg` argument at a call site (Kotlin IR's `IrVararg`): the spread/listed elements and
    /// their element type. The JVM backend packs them into an array; another backend may differ.
    Vararg {
        /// The whole array type (`kotlin/IntArray`, `kotlin/Array<Int>`, `kotlin/Array<String>`), NOT the
        /// bare element — the JVM emitter derives the element + `newarray`/`anewarray` (and boxing of a
        /// `kotlin/Array<Int>` = `Integer[]`) from it via `ir_ty_to_jvm`. The element alone is ambiguous
        /// (`Obj("kotlin/Int")` is both a primitive `IntArray` element and a boxed `Array<Int>` element).
        array_type: Ty,
        elements: Vec<ExprId>,
    },
    /// Allocate an uninitialized array of `size` elements (`anewarray` for a reference element,
    /// `newarray` for a primitive) — the sized constructor `Array<T>(n) { … }` / `arrayOfNulls<T>(n)`
    /// fills it afterwards. (`Vararg` is the *literal* form with a statically-known element list.)
    NewArray {
        /// The whole array type — see [`IrExpr::Vararg::array_type`].
        array_type: Ty,
        size: ExprId,
    },
    /// `try { body } catch (e: E) { … } … [finally { f }]`. `result` is the value type (`Unit` when
    /// used as a statement). Each catch binds the exception to a value index and runs its body. A
    /// `finally` block runs on every exit (normal, each catch, and an uncaught exception via a
    /// catch-all that re-throws); it is emitted (inlined) at each.
    Try {
        body: ExprId,
        catches: Vec<IrCatch>,
        finally: Option<ExprId>,
        result: Ty,
    },
}

/// One `catch (var: exc_internal) { body }` clause of an [`IrExpr::Try`].
#[derive(Clone, Debug)]
pub struct IrCatch {
    /// Value index the caught exception is bound to.
    pub var: u32,
    /// JVM internal name of the caught exception type.
    pub exc_internal: String,
    pub body: ExprId,
}

/// Built-in binary operators carried by `IrExpr::PrimitiveBinOp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    /// Referential identity (`===`/`!==`): a JVM `if_acmp*` on two reference operands, never the
    /// structural `Intrinsics.areEqual` that `==`/`!=` (`Eq`/`Ne`) uses for references.
    RefEq,
    RefNe,
    And,
    Or,
    /// Bitwise/shift on `Int`/`Long` (Kotlin's `and`/`or`/`xor`/`shl`/`shr`/`ushr` infix functions).
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Ushr,
}

/// The `IrTypeOperatorCall` operators (Kotlin IR's `IrTypeOperator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrTypeOp {
    InstanceOf,    // `is T`
    NotInstanceOf, // `!is T`
    Cast,          // `as T?` (or `as <primitive>`): a plain `checkcast` — `null` passes
    /// `as T` to a non-null reference type: null-check (`Intrinsics.checkNotNull`) then `checkcast`,
    /// so casting `null` throws — matching kotlinc.
    CastNonNull,
    SafeCast, // `as? T`
    /// Representation coercion the backend inserts (e.g. JVM box/unbox) — explicit in the IR so it
    /// is visible and testable, not hidden in codegen.
    ImplicitCoercion,
}

/// A function/method declaration (`IrFunction`).
#[derive(Clone, Debug)]
pub struct IrFunction {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
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

/// One entry of an `enum class` in [`IrClass`]. Groups what were parallel `Vec`s keyed by entry index
/// (the `(name, args)` tuple plus the separate `subclass` vec), so an entry's name / constructor args /
/// synthesized-subclass marker can't desync.
#[derive(Clone, Debug)]
pub struct IrEnumEntry {
    /// Entry name (`RED`).
    pub name: String,
    /// Lowered constructor-argument value ids (`RED(0xFF0000)`); empty for an arg-less entry. Filled in a
    /// later lowering pass — built empty when the entry list is first created.
    pub args: Vec<ExprId>,
    /// `Some(subclass_fq)` when the entry has a body and is constructed as an instance of a synthesized
    /// anonymous subclass (`new Enum$ENTRY(name, ordinal, args)`); `None` when constructed as the enum
    /// itself.
    pub subclass: Option<String>,
}

/// One instance field of an [`IrClass`]. Groups what were parallel `Vec`s keyed by field index, so a
/// field's type / generic-param name / constant default / finality / visibility can't desync.
#[derive(Clone, Debug)]
pub struct IrField {
    pub name: String,
    pub ty: Ty,
    /// The source type-parameter NAME the field was declared with (`val x: T` → `Some("T")`), else
    /// `None`. Platform-neutral; lets the value-class pass pick the CORRECT bound for a generic
    /// underlying (vs guessing), independent of erasure dropping the name.
    pub type_param: Option<String>,
    /// The CONSTANT default from a primary-constructor default (`val b: Int = 5` → `Some(Int(5))`,
    /// `val t: T? = null` → `Some(Null)`), else `None` (no default, or a non-constant one). The
    /// serialization extension marks an element with a default `isOptional`; ignored by the core backend.
    pub default: Option<IrConst>,
    /// The backing field is immutable (`val`) — emitted `final`.
    pub is_final: bool,
    /// Private backing field — the Kotlin default (reached via accessors). `false` for a non-private
    /// field read/written cross-class (a coroutine continuation's `result`/`label`). Each backend maps
    /// this to its own access representation (the JVM emitter → `ACC_PRIVATE`/`ACC_PUBLIC`).
    pub is_private: bool,
    /// Backs a `lateinit var`. EVERY read of such a field (a backend `GetField`) null-checks it and
    /// throws `UninitializedPropertyAccessException` when still unset — matching kotlinc, which inserts
    /// the check at each access site (not only the property getter).
    pub is_lateinit: bool,
}

impl IrField {
    /// A plain backing field with Kotlin defaults: mutable-unknown (`is_final = false`), `private`, no
    /// generic-param name, no constant default. Synthesized classes build fields from this.
    pub fn new(name: String, ty: Ty) -> IrField {
        IrField {
            name,
            ty,
            type_param: None,
            default: None,
            is_final: false,
            is_private: true,
            is_lateinit: false,
        }
    }
}

/// One primary-constructor parameter of an [`IrClass`], in declaration order. Folds what were the
/// index-parallel `ctor_args` tuple and `ctor_param_checks` vec, so a parameter's type / `is_field`
/// flag / null-check name can't desync.
#[derive(Clone, Debug)]
pub struct IrCtorArg {
    /// The parameter type (carries declared nullability — a nullable value-class param erases like its
    /// field).
    pub ty: Ty,
    /// `true` ⇒ a `val`/`var` property whose arg is stored to a field (the property fields are
    /// `fields[0..]` in the same relative order); `false` ⇒ a plain parameter, an argument only,
    /// available as a local in `<init>` for property initializers / `init` blocks.
    pub is_field: bool,
    /// `Some(name)` when the backend should guard this parameter with a non-null assertion
    /// (`Intrinsics.checkNotNullParameter`) at `<init>` entry — a non-null reference param. `None` for a
    /// primitive, nullable, or class-type-parameter param, and for the synthetic inner `this$0`.
    pub check: Option<String>,
}

/// A class/interface/object declaration (`IrClass`). Instance fields come from the primary
/// constructor's `val`/`var` parameters (in order); the constructor stores each.
#[derive(Clone, Debug)]
pub struct IrClass {
    pub fq_name: String,
    /// `@JvmInline value class` — a single-field class represented unboxed (as its one field's type) by
    /// the JVM `jvm::value_classes` IR pass. The IR otherwise treats it as a plain class.
    pub is_value: bool,
    /// Declared non-`Any` generic upper bounds (`<T: String>` → `("T", String)`), carried verbatim from
    /// the source. Platform-neutral metadata; the JVM value-class pass uses it to erase a value class's
    /// underlying type parameter to its bound (`value class S<T: String>` → `String`).
    pub type_param_bounds: Vec<(String, Ty)>,
    /// ALL declared generic type-parameter names in order (`class C<A, B>` → `["A","B"]`), including
    /// those with only the implicit `Any` bound (unlike [`type_param_bounds`], which lists only non-`Any`
    /// bounds). The serialization extension uses the count/order to shape a generic `$serializer`
    /// (one `KSerializer` constructor arg per type parameter). Empty for a non-generic class.
    pub type_params: Vec<String>,
    pub supertypes: Vec<Ty>,
    /// Instance fields. The first `ctor_param_count` are the primary-constructor parameters (stored
    /// directly from args, in order); any after them are class-body properties initialized by `init_body`.
    pub fields: Vec<IrField>,
    /// Per-property serial-name overrides from `@SerialName("x")` on a constructor property, as
    /// `(property_name, serial_name)`. Empty for a class with no such annotation. Consumed by the
    /// serialization extension to name descriptor elements; ignored by the core backend.
    pub serial_names: Vec<(String, String)>,
    /// The internal name of an explicit serializer from `@Serializable(with = X::class)`, or `None`.
    /// The serialization extension makes `serializer()` return an instance of `X` instead of generating
    /// a default `$serializer`. Ignored by the core backend.
    pub custom_serializer: Option<String>,
    /// Per-property explicit element serializers from `@Serializable(with = X::class)` on a constructor
    /// property, as `(property_name, serializer_internal)`. Empty for a class with no such annotation.
    /// The serialization extension routes that property's `childSerializers`/descriptor element through
    /// an instance of `X` instead of a default builtin/nested serializer. Ignored by the core backend.
    pub field_serializers: Vec<(String, String)>,
    /// Property names whose element serializer is CONTEXTUAL (`@Contextual` on the property, or the file
    /// carries `@file:UseContextualSerialization(<the property's type>::class)`). The serialization
    /// extension emits `ContextualSerializer(<type>::class)` (descriptor kind CONTEXTUAL) for these
    /// instead of a derived builtin/nested serializer. Empty for a class with no contextual property.
    pub contextual_fields: Vec<String>,
    /// How many leading `fields` are property constructor parameters (`val`/`var`) — the rest are body
    /// properties. NOTE: this is the count of constructor params that BACK A FIELD, not the total
    /// constructor arity (a non-`val`/`var` parameter is an argument only, no field) — see `ctor_args`.
    pub ctor_param_count: u32,
    /// ALL primary-constructor parameters in declaration order (each an [`IrCtorArg`] with type,
    /// `is_field`, and optional null-check name). Empty for synthesized/enum/object classes (then the
    /// constructor arity is `ctor_param_count`).
    pub ctor_args: Vec<IrCtorArg>,
    /// Constructor body run after `super(…)`: an effect `Block` lowered with `this` = value 0 and the
    /// constructor parameters as values `1..=N`. When [`explicit_param_stores`] is set it BEGINS with the
    /// `val`/`var` param→field stores (the desugared primary-constructor sugar); it also carries body-
    /// property initializers (`SetField`) and `init { … }` blocks. `None` when there's nothing to run.
    pub init_body: Option<ExprId>,
    /// `true` when `init_body` already stores the primary-constructor `val`/`var` params (and inner
    /// `this$0`) to their fields — the desugared form. The JVM backend then must NOT auto-store them (it
    /// would double-store). `false` for synthesized classes that still rely on the backend's implicit
    /// param→field store.
    pub explicit_param_stores: bool,
    /// Instance methods — `FunId`s into `IrFile.functions` (each with `dispatch_receiver = Some`).
    pub methods: Vec<FunId>,
    pub is_interface: bool,
    /// `true` for a Kotlin `annotation class`. Emitted as a JVM annotation INTERFACE (`ACC_ANNOTATION|
    /// ACC_INTERFACE|ACC_ABSTRACT`, extends `java/lang/annotation/Annotation`, one abstract accessor per
    /// member named after the property — from `fields`). NOT a plain class.
    pub is_annotation: bool,
    /// `Some(annotation_interface_internal)` when this class is the synthetic IMPLEMENTATION of an
    /// annotation (kotlinc's `…$annotationImpl$A$0`): it implements the annotation interface and the JVM
    /// `java.lang.annotation.Annotation` contract (per-member accessors + content `equals`/`hashCode`/
    /// `toString`/`annotationType`), so `A(args)` can construct an annotation instance. `fields` are the
    /// members in order. The backend emits the whole contract from `fields`.
    pub annotation_impl_of: Option<String>,
    /// `true` for a `sealed class`/`sealed interface`. The serialization extension routes a sealed
    /// `@Serializable` base's `serializer()` to a runtime `SealedClassSerializer` over its `@Serializable`
    /// subclasses (polymorphic), instead of generating an empty `$serializer`. Ignored by the core backend.
    pub is_sealed: bool,
    /// `true` for an `abstract class` (not `sealed`). The serialization extension serializes a property
    /// of an abstract `@Serializable` type via a runtime `PolymorphicSerializer(<type>::class)` (open
    /// polymorphism), since an abstract base has no closed subclass set. Ignored by the core backend.
    pub is_abstract: bool,
    /// Semantic superclass internal name (`kotlin/Any` normally, or a user base class for
    /// `class B : A(args)`). Target-specific representation classes such as JVM enum bases are chosen by
    /// the backend.
    pub superclass: String,
    /// Arguments to the base-class constructor (`: A(args)`) — lowered IR value ids, evaluated with
    /// `this`=value 0 and the primary-constructor params as values `1..=ctor_param_count`. Empty
    /// unless `superclass` is a user base class.
    pub super_args: Vec<ExprId>,
    /// Enum entries in declaration order. Non-empty only for an `enum class`; the backend emits a static
    /// field per entry, a `$VALUES` array, a `<clinit>` that constructs them, and `values()`/
    /// `valueOf(String)`. Each [`IrEnumEntry`] carries its name, lowered constructor args, and optional
    /// synthesized-subclass fq name.
    pub enum_entries: Vec<IrEnumEntry>,
    /// `Some(user_field_types)` marks this class as a synthesized enum-entry subclass: it extends the
    /// enum (`superclass`), has no own fields, and its constructor is `(String name, int ordinal,
    /// <user_field_types>)V` delegating to the enum's `(String,int,<user>)V` constructor.
    pub enum_entry_of: Option<Vec<Ty>>,
    /// `Some(..)` marks this class as a synthesized property-reference singleton: a `final class
    /// extends kotlin/jvm/internal/PropertyReference1Impl` (the `superclass`) with a `public static
    /// final INSTANCE`, a constructor `super(owner.class, name, signature, 0)`, and a `get(Object)
    /// Object` override that reads the referenced property via its getter.
    pub prop_ref: Option<PropRef>,
    /// When `Some`, this class is a synthesized function-reference subclass (`<Owner>$ref$N extends
    /// kotlin/jvm/internal/FunctionReferenceImpl implements Function<arity>`), emitted by
    /// `emit_func_ref_class`. Gives callable references real Kotlin reference EQUALITY (the base class
    /// compares owner/name/signature/boundReceiver) — `::f == ::f`, `a::m != b::m`.
    pub func_ref: Option<FuncRef>,
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
    /// `true` for a synthesized `C$Companion` class: a private no-arg constructor and no own singleton
    /// field (the `Companion` instance is held by the outer class).
    pub is_companion: bool,
    /// `Some(companion_fq)` on a class with a `companion object`: emit a `public static final
    /// <companion> Companion` field, initialized in this class's `<clinit>`.
    pub companion_class: Option<String>,
    /// Secondary constructors — each an extra `<init>(params)` that delegates to the primary
    /// constructor (`constructor(…) : this(args)`) then runs its body. Empty for most classes.
    pub secondary_ctors: Vec<IrSecondaryCtor>,
    /// `false` for a class with NO primary constructor: the backend emits no primary `<init>`; every
    /// `<init>` comes from `secondary_ctors` (a `Super`-delegating one carries the init body). `true`
    /// for every other class (including synthesized/enum/object).
    pub has_primary_ctor: bool,
    /// RUNTIME-retained annotations applied to this class (`@Anno(...) class TTT`), emitted into the
    /// class's `RuntimeVisibleAnnotations` attribute. Empty for a class with none.
    pub applied_annotations: Vec<AppliedAnnotation>,
    /// For an `annotation class`: `true` when its Kotlin retention is RUNTIME (the default) — the emitter
    /// then writes a `@java.lang.annotation.Retention(RUNTIME)` meta-annotation on the annotation interface
    /// so the JVM keeps the annotation's uses visible to reflection.
    pub runtime_retained: bool,
}

/// A resolved JVM annotation value (`element_value`, JVMS §4.7.16.1) — an annotation argument folded to
/// the constant the class file encodes.
#[derive(Clone, Debug)]
pub enum AnnoValue {
    /// A primitive/`String` constant (encoded by tag `B`/`C`/`D`/`F`/`I`/`J`/`S`/`Z`/`s`).
    Const(IrConst),
    /// An enum constant `(enum_type_internal, const_name)` — tag `e`.
    Enum(String, String),
    /// A class literal `T::class` `(type_internal)` — tag `c` (its type descriptor).
    Class(String),
    /// A nested annotation instance `A(...)` — tag `@`.
    Annotation(AppliedAnnotation),
    /// An array `[…]` — tag `[`.
    Array(Vec<AnnoValue>),
}

/// An applied annotation (`@Anno(...)`) to encode into a `RuntimeVisibleAnnotations` attribute.
#[derive(Clone, Debug)]
pub struct AppliedAnnotation {
    /// The annotation type's internal name (`Anno`).
    pub internal: String,
    /// `element_value_pairs`: `(element_name, value)` in declaration order.
    pub values: Vec<(String, AnnoValue)>,
}

/// How a function-reference subclass's `invoke` dispatches to its target.
#[derive(Clone, Debug)]
pub enum FrDispatch {
    /// Top-level / static target: `invokestatic call_owner.call_name(call_desc)`. All invoke params are
    /// the call arguments.
    Static,
    /// Unbound member `Type::m`: the FIRST invoke param is the receiver; `invokevirtual` on it.
    VirtualUnbound,
    /// Bound member `obj::m`: the receiver is captured (`this.receiver`); `invokevirtual` on it. All
    /// invoke params are the call arguments.
    VirtualBound,
    /// Bound extension `obj::ext`: the receiver is captured (`this.receiver`) and passed as the FIRST
    /// argument of `invokestatic call_owner.call_name(receiver, args…)`. `target_param_tys` leads with
    /// the receiver type; `param_tys` (the invoke args) map to `target_param_tys[1..]`.
    StaticBound,
}

/// A synthesized function-reference subclass of `kotlin/jvm/internal/FunctionReferenceImpl`. See
/// `emit_func_ref_class`. `param_tys`/`ret_ty` are the LOGICAL `invoke` signature (for `VirtualUnbound`,
/// `param_tys[0]` is the receiver); the SAM interface erases them to `Object`, so `invoke` casts.
#[derive(Clone, Debug)]
pub struct FuncRef {
    pub bound: bool,
    pub arity: u8,
    /// Class passed to `super(...)` (the reference's declaring class); empty = the file facade.
    pub owner_class: String,
    pub fn_name: String,
    pub flags: i32,
    pub dispatch: FrDispatch,
    /// Class the target method is invoked on; empty = the file facade.
    pub call_owner: String,
    pub call_name: String,
    /// The target method is declared on an INTERFACE (`invokeinterface`, not `invokevirtual`).
    pub call_interface: bool,
    /// The LOGICAL `invoke` parameter types. For `VirtualUnbound`, `param_tys[0]` is the receiver
    /// (excluded from the method descriptor / signature). The emitter derives the JVM signature and
    /// reference metadata signature from these + `ret_ty`.
    pub param_tys: Vec<Ty>,
    pub ret_ty: Ty,
    /// The PHYSICAL target-call parameter/return types after backend lowerings such as JVM value-class
    /// erasure. Same shape as `param_tys` (including the unbound receiver slot when present).
    pub target_param_tys: Vec<Ty>,
    pub target_ret_ty: Ty,
    /// Per logical invoke parameter: `Some(value_class_internal)` means the erased Object argument is a
    /// boxed value-class instance and must be unboxed before the physical target call.
    pub unbox_params: Vec<Option<String>>,
    /// Parallel to `unbox_params`: nullable value-class parameters unbox `null` to a null underlying.
    pub unbox_param_nullable: Vec<bool>,
    /// `Some(value_class_internal)` means the physical target returns the value-class underlying and the
    /// function-reference `invoke` must box it back before returning Object.
    pub box_ret: Option<String>,
    /// `StaticBound` only: `Some(value_class_internal)` when the CAPTURED receiver is a value class
    /// (`Z(42)::ext`). The receiver is stored boxed as `Object`; the emitter `checkcast`s it to the box
    /// class then `unbox-impl`s it to the underlying before the mangled `invokestatic ext-<hash>(under)`.
    pub staticbound_recv_unbox: Option<String>,
}

/// A synthesized property-reference class's metadata (`Type::prop` → `Type$prop$N`): the referenced
/// property's owner, name, getter, and value type. The backend emits the `PropertyReference1Impl`
/// subclass from this.
#[derive(Clone, Debug)]
pub struct PropRef {
    pub owner_internal: String,
    pub prop_name: String,
    pub getter_name: String,
    pub prop_ty: Ty,
    /// `false` = an unbound `Type::prop` (a `PropertyReference1Impl` singleton with `get(Object)`);
    /// `true` = a bound `obj::prop` (a `PropertyReference0Impl` constructed with the captured receiver,
    /// whose `get()` reads `this.receiver`).
    pub bound: bool,
    /// A top-level property reference `::foo` (a `(Mutable)PropertyReference0Impl` singleton): the
    /// getter/setter are STATIC on the file facade, so `get`/`set` dispatch via `invokestatic`
    /// (`owner_internal` empty = the facade sentinel, resolved at emit). No receiver is captured.
    pub static_dispatch: bool,
    /// The referenced property is a `var` — emit a `set(Object)` override (calls `setName`). Only
    /// meaningful with `static_dispatch` (a `MutablePropertyReference0Impl`).
    pub mutable: bool,
    /// An EXTENSION property reference (`obj::ext`, `Type::ext` where `val Recv.ext`): the getter/setter
    /// are STATIC methods on this facade taking the receiver as the first argument (`getExt(Recv)` /
    /// `setExt(Recv, v)`), unlike a member reference's instance `getExt()`. `None` for member/top-level
    /// references. The reference's receiver-class metadata still lives in `owner_internal`.
    pub ext_facade: Option<String>,
}

/// A secondary constructor: `<init>(params)` evaluates `delegate_args`, calls the delegate target
/// (`invokespecial`), then runs `body`. `this` is value 0 and the parameters are values
/// `1..=params.len()` in `delegate_args`/`body`.
#[derive(Clone, Debug)]
pub struct IrSecondaryCtor {
    pub params: Vec<Ty>,
    pub delegate_args: Vec<ExprId>,
    pub body: Option<ExprId>,
    /// Which `<init>` this constructor delegates to, and whether it runs the class init body.
    pub delegate: CtorDelegateTarget,
}

/// The delegation target of a secondary constructor.
#[derive(Clone, Debug)]
pub enum CtorDelegateTarget {
    /// `this(args)` → `invokespecial` an own `<init>(target_params)` (the primary, or a sibling
    /// secondary). The class init body runs in the reached constructor, not here. `to_primary` marks
    /// a delegation to the PRIMARY `<init>`, whose live (post-value-class-pass) signature the emitter
    /// reads directly — `target_params` is the lower-time signature, correct only for a sibling target.
    This {
        target_params: Vec<Ty>,
        to_primary: bool,
    },
    /// `super(args)` (or implicit) in a class with NO primary constructor → `invokespecial` the
    /// superclass `<init>` (its signature is read live from the base class at emit time), then run the
    /// class init body (field initializers + `init {}`) before this constructor's own `body`.
    Super,
}

/// A synthetic bridge method (`name(erased_params)erased_ret` → `name(concrete_params)concrete_ret`).
#[derive(Clone, Debug)]
pub struct Bridge {
    pub name: String,
    pub erased_params: Vec<Ty>,
    pub erased_ret: Ty,
    pub concrete_params: Vec<Ty>,
    pub concrete_ret: Ty,
    /// The method this bridge delegates to, when it differs from `name` — a value-class-returning
    /// override is emitted under a mangled name (`foo-<hash>`), so the unmangled bridge (`foo`, the
    /// supertype's erased signature) must call the mangled one. `None` ⇒ same as `name`.
    pub target_name: Option<String>,
    /// When set, the bridge boxes its (unboxed value-class) result with `<owner>.box-impl` before
    /// returning — a value-class-returning override seen through a supertype hands back a boxed `X`.
    pub box_ret: Option<String>,
    /// Per concrete parameter, the boxed value class to `checkcast` + `unbox-impl` before the target
    /// call — a generic supertype method (`B.f(T,U)` → erased `f(Object,Object)`) delegates to a
    /// mangled concrete override taking the value class's UNDERLYING, while the incoming arg is a
    /// boxed `X`. Empty (or all-`None`) ⇒ plain checkcast/convert (the common case). JVM/value-class
    /// concern, populated by the value-class pass; the front end leaves it empty.
    pub unbox_params: Vec<Option<String>>,
}

/// A top-level (module) property: a static field on the file facade, initialized in `<clinit>`.
#[derive(Clone, Debug)]
pub struct IrStatic {
    pub name: String,
    pub ty: Ty,
    /// The initializer expression (run in `<clinit>` in declaration order).
    pub init: ExprId,
    /// `var` (mutable) ⇒ a setter is emitted and the backing field is non-`final`.
    pub is_var: bool,
    /// `const val` ⇒ kotlinc keeps the field `public static final` (inlined at use) with no accessor;
    /// a plain top-level `val`/`var` is `private static [final]` + a `public static` getter/setter.
    pub is_const: bool,
    /// The class this static field belongs to. `None` = the file facade (a top-level property). `Some`
    /// = a specific class — a `companion object`'s `const val` lives on the OUTER class (kotlinc emits
    /// `public static final` + `ConstantValue` there), not the facade.
    pub owner: Option<String>,
    /// `true` when this backing field has a CUSTOM accessor (`val x = init get() = field…`): the field
    /// is still emitted + initialized in `<clinit>`, but the trivial `getX`/`setX` accessors are NOT
    /// auto-generated here — the custom `getX`/`setX` are emitted as ordinary facade methods (their
    /// bodies lowered with `field` bound to this static). Prevents a duplicate-accessor collision.
    pub custom_accessor: bool,
}

#[derive(Clone, Default, Debug)]
pub struct FnParamInfo {
    pub names: Vec<String>,
    pub defaults: Option<Vec<Option<ExprId>>>,
}

impl FnParamInfo {
    pub fn names(names: Vec<String>) -> Self {
        Self {
            names,
            defaults: None,
        }
    }

    pub fn defaults(names: Vec<String>, defaults: Vec<Option<ExprId>>) -> Self {
        Self {
            names,
            defaults: Some(defaults),
        }
    }
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
    /// `ExprId` → the expression's LOGICAL (source) type as the checker inferred it, recorded verbatim by
    /// the lowerer — NOT erased. The value-class pass consults it to recover the representation of a value
    /// whose IR node alone is ambiguous: a library call returns a physical `Object` descriptor, but its
    /// logical type may be a value class (`runCatching{…}: Result`), so the pass knows the result is the
    /// value class's UNBOXED underlying, not an opaque `Object`. Populated for every lowered expression;
    /// consumed ONLY by the value-class pass (the sole owner of value-class knowledge).
    pub logical_types: std::collections::HashMap<u32, Ty>,
    /// `FunId` → source parameter names and, when present, default-value expressions.
    pub fn_params: std::collections::HashMap<u32, FnParamInfo>,
    /// Instance methods kotlinc leaves NON-`final` even in a final class — currently the data-class
    /// `Object`-overrides (`toString`/`hashCode`/`equals`), which kotlinc emits `public` (open) rather
    /// than `public final`. The JVM backend omits `ACC_FINAL` for a `FunId` in this set.
    pub open_methods: std::collections::HashSet<u32>,
    /// Instance methods kotlinc emits `private` — currently a property's `private set` setter. The JVM
    /// backend uses `ACC_PRIVATE` instead of `ACC_PUBLIC` for a `FunId` in this set.
    pub private_methods: std::collections::HashSet<u32>,
    /// Lambda impl functions that are INLINE-ONLY — their body has a non-local `return` (returning from
    /// the enclosing function), which is valid only when the lambda is spliced at the call site, never as
    /// a standalone closure method (a non-local return can't compile to a separate method — its `areturn`
    /// would carry the enclosing fn's return type, mismatching the lambda's). The splice reads the
    /// lambda's `inline_body`, not this method, so the backend must NOT emit a `FunId` in this set.
    pub inline_only_fns: std::collections::HashSet<u32>,
    /// `FunId`s of `suspend fun`s, tagged by ir_lower. The coroutine pass (`jvm::suspend`) owns the
    /// whole transform: it rewrites each to the continuation-passing-style ABI (an extra
    /// `kotlin.coroutines.Continuation` parameter, return type erased to `Object`) and, for a function
    /// with suspension points, builds the state machine + continuation class. ir_lower itself lowers a
    /// `suspend fun` as a plain function (mirroring how value classes stay plain until their pass).
    pub suspend_funs: Vec<u32>,
    /// `ExprId` of each direct call to a `suspend fun` → the callee's LOGICAL return type (the source
    /// return, before CPS erasure to `Object`). Recorded by ir_lower from the resolver
    /// (`flags.suspend`), so the coroutine pass recognizes a suspend call to ANOTHER file or a classpath
    /// dependency — whose `FunId` is absent from this file's `suspend_funs`. Same-file/member suspend
    /// calls are caught by `suspend_funs`; this is the cross-unit complement.
    pub suspend_calls: std::collections::HashMap<u32, Ty>,
    /// A `suspend` LAMBDA's `invokeSuspend` that contains MULTIPLE suspensions / control flow and needs
    /// a state machine with the lambda instance itself as the continuation — `(invokeSuspend FunId,
    /// lambda ClassId, field_base)`. `field_base` is the first free field index on the lambda class
    /// (after its captures/parameters), where the coroutine pass appends the `result`/`label`/spilled
    /// fields. ir_lower builds `invokeSuspend` with the plain body (suspend calls un-threaded); the pass
    /// flattens it. (Single-suspension lambdas are handled inline by ir_lower instead.) `field_base` is
    /// the number of leading capture/parameter fields — the pass reloads them into locals `2..` at each
    /// `invokeSuspend` entry (so a captured/parameter value survives a re-entry), excludes them from
    /// spilling, and places the result/label/spilled fields after them.
    pub suspend_lambda_sm: Vec<(u32, u32, u32)>,
    /// `FunId` → the backend-agnostic generic-signature SHAPE of a type-parameterized function. The JVM
    /// backend formats this into a `Signature` attribute; the IR itself holds no target descriptors.
    pub signatures: std::collections::HashMap<u32, IrGenericSig>,
    /// Class fq-internal-name → its generic-signature SHAPE (type parameters + bounds), for a generic
    /// class. The JVM backend formats it into the class `Signature` attribute.
    pub class_signatures: std::collections::HashMap<String, IrGenericSig>,
    /// Class fq-internal-name → `(field name, type-parameter name)` for each field whose declared type
    /// is a bare type parameter (`class Pair<A, B>(val a: A)` → `[("a", "A")]`). The JVM backend formats
    /// each into a field `Signature` (`TA;`). Backend-agnostic: only the type-parameter name is stored.
    pub field_signatures: std::collections::HashMap<String, Vec<(String, String)>>,
    /// Classpath `@JvmInline value class` (fq-internal-name → erased underlying `Ty`) REFERENCED in
    /// this file — `kotlin/Result` → `Object`. The JVM value-class pass merges these into its erasure map
    /// so a classpath value-class type unboxes exactly like a user value class. Populated by ir_lower
    /// (which has the classpath); only REFERENCE-underlying ones are recorded (a primitive-underlying
    /// `UInt`/`ULong` keeps its existing dedicated handling).
    pub external_value_classes: std::collections::HashMap<String, Ty>,
    /// Getter method name (`getV`) for each classpath `@JvmInline value class` in
    /// [`Self::external_value_classes`] — lets the value-class pass recognize a sole-property read emitted
    /// as `invokevirtual X.getV()` and rewrite it to identity (the receiver IS the unboxed underlying).
    pub external_value_class_getters: std::collections::HashMap<String, String>,
    /// Call `ExprId` → reified-type substitution for a `<reified T>` CLASSPATH inline extension whose
    /// compiled body the backend must splice: `[(type-parameter name, concrete JVM internal name)]`
    /// (`[("T", "lib/Prov")]`). The bytecode splicer feeds this to `substitute_reified` so a
    /// `reifiedOperationMarker`/`T::class` in the spliced body specializes to the concrete type — the
    /// classpath analogue of the IR inliner's `reified_subst` (which only has same-file bodies). The
    /// concrete type is a backend-agnostic `Ty`; the JVM splicer maps it to an internal name.
    pub reified_call_subst: std::collections::HashMap<u32, Vec<(String, Ty)>>,
    /// Extension-call `ExprId` → the extension's DECLARED (un-erased) receiver source type, forwarded
    /// verbatim from the resolved callable's `source_receiver`. `ir_lower` records it with NO value-class
    /// reasoning of its own; the value-class pass reads it to decide box/unbox at the receiver. The signal
    /// distinguishes `fun Result<T>.getOrThrow()` (receiver `kotlin/Result` — a value class whose facade
    /// method takes the UNBOXED underlying, so a `Boxed` receiver unboxes) from a generic `fun <T> T.foo()`
    /// (receiver a type variable — erases to `Object`, receiver stays boxed) even though both erase
    /// identically in the JVM descriptor. Only concrete declared receivers are recorded (a `Var` receiver
    /// is `None` at the source and never inserted).
    pub ext_call_source_receiver: std::collections::HashMap<u32, Ty>,
    /// Lifted-lambda function id → the parameter INDEX at which the lambda's OWN parameters begin (its
    /// captured variables occupy the lower indices). A lambda's own parameters arrive through the
    /// `FunctionN` generic (`Object`) invoke slot, so a reference-underlying value-class parameter is
    /// BOXED there — the value-class pass reads this to type such a slot as the boxed value class (so
    /// `it.getOrThrow()` unboxes it), without the lowerer probing value-class-ness itself.
    pub lambda_own_params_from: std::collections::HashMap<u32, u32>,
}

/// Backend-agnostic generic-signature shape of a declaration (the data a JVM `Signature` / a future
/// platform's equivalent needs). NO target descriptors here — each backend formats its own.
#[derive(Clone, Debug)]
pub struct IrGenericSig {
    /// Each type parameter: its name and its upper bound as a Kotlin `Ty` (`kotlin/Any` when none).
    pub type_params: Vec<(String, Ty)>,
    /// Per value parameter: `Some(name)` when it is a bare type-parameter reference, else `None` (the
    /// backend uses the parameter's own erased type). Empty for a class signature.
    pub param_tparams: Vec<Option<String>>,
    /// `Some(name)` when the return type is a bare type-parameter reference, else `None`.
    pub ret_tparam: Option<String>,
    /// For a CLASS signature with a PARAMETERIZED supertype: the superclass + superinterfaces as
    /// platform-agnostic `Ty`s carrying their type arguments (`[Any, Operation<Result<Int>>]`), so a
    /// cross-module reader recovers a member's concrete generic return. The backend formats these into the
    /// JVM `Signature` string. Empty ⇒ no parameterized supertype (backend emits the default `Object`
    /// superclass). Empty for a function signature.
    pub supers: Vec<Ty>,
}

impl IrFile {
    pub fn param_defaults(&self, fid: u32) -> Option<&Vec<Option<ExprId>>> {
        self.fn_params.get(&fid)?.defaults.as_ref()
    }
    pub fn has_param_defaults(&self, fid: u32) -> bool {
        self.param_defaults(fid).is_some()
    }
    pub fn param_names(&self, fid: u32) -> Option<&[String]> {
        Some(&self.fn_params.get(&fid)?.names)
    }
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

/// Invoke `f` on each direct child expression of `e`. The single structural definition of an
/// `IrExpr`'s sub-expressions — tree walks (index shifting, scans) delegate here so a new variant is
/// covered in one place. Written EXHAUSTIVELY (no `_` arm) on purpose: adding an `IrExpr` variant must
/// fail to compile here rather than silently drop its children from every walk.
pub fn for_each_child(exprs: &[IrExpr], e: ExprId, f: &mut impl FnMut(ExprId)) {
    match &exprs[e as usize] {
        IrExpr::Block { stmts, value } => {
            stmts.iter().for_each(|&s| f(s));
            value.iter().for_each(|&v| f(v));
        }
        IrExpr::When { branches } => branches.iter().for_each(|(c, b)| {
            c.iter().for_each(|&c| f(c));
            f(*b);
        }),
        IrExpr::Return(v) => v.iter().for_each(|&v| f(v)),
        IrExpr::TypeOp { arg, .. }
        | IrExpr::NotNullAssert { operand: arg }
        | IrExpr::LateinitCheck { operand: arg, .. }
        | IrExpr::Throw { operand: arg }
        | IrExpr::EnumValueOf { arg, .. }
        | IrExpr::RefNew { init: arg, .. }
        | IrExpr::RefGet { holder: arg, .. }
        | IrExpr::NewArray { size: arg, .. } => f(*arg),
        IrExpr::StringConcat(parts) => parts.iter().for_each(|&p| f(p)),
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            f(*lhs);
            f(*rhs);
        }
        IrExpr::SetValue { value, .. } | IrExpr::SetStatic { value, .. } => f(*value),
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            f(*receiver);
            f(*value);
        }
        IrExpr::Variable { init, .. } => init.iter().for_each(|&i| f(i)),
        IrExpr::GetField { receiver, .. } => f(*receiver),
        IrExpr::Call {
            args,
            dispatch_receiver,
            ..
        } => {
            dispatch_receiver.iter().for_each(|&r| f(r));
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            f(*receiver);
            args.iter().flatten().for_each(|&a| f(a));
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            f(*func);
            args.iter().for_each(|&a| f(a));
        }
        IrExpr::New { args, .. }
        | IrExpr::NewExternal { args, .. }
        | IrExpr::NewCrossFile { args, .. }
        | IrExpr::Vararg { elements: args, .. } => args.iter().for_each(|&a| f(a)),
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter().for_each(|&c| f(c));
            inline_body.iter().for_each(|&b| f(b));
        }
        IrExpr::While {
            cond, body, update, ..
        } => {
            f(*cond);
            f(*body);
            update.iter().for_each(|&u| f(u));
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            f(*body);
            catches.iter().for_each(|c| f(c.body));
            finally.iter().for_each(|&fin| f(fin));
        }
        IrExpr::PluginPlaceholder { exprs: kids, .. } => kids.iter().for_each(|&k| f(k)),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::GetValue(_)
        | IrExpr::GetStatic(_)
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation => {}
    }
}

/// Whether a top-level `foo$default` synthetic can be SAFELY emitted for `fid`. The function name must be
/// unmangled — a value-class-parameter-mangled `foo-<hash>` needs box/unbox adaptation the plain facade
/// stub doesn't model — and every registered default expression must be simple enough to re-emit inside
/// the stub: no lambda, no `invoke`, no value-class-mangled call, and no reference to a value index beyond
/// the parameters (a default that spilled a temp or captured a closure). A plain OBJECT/`new` construction
/// (`filters: F = F()`) IS allowed — the stub re-emits it like any other value. Conservative — an unknown
/// shape is rejected, so the caller falls back to the unchanged inline call-site fill (never a miscompile).
pub fn toplevel_default_stub_safe(ir: &IrFile, fid: u32) -> bool {
    let f = &ir.functions[fid as usize];
    if f.name.contains('-') {
        return false;
    }
    // A user function literally named `<name>$default` (a back-ticked identifier) would collide with the
    // synthetic — don't emit the stub (kotlinc also treats that as a conflicting declaration).
    let stub_name = format!("{}$default", f.name);
    if ir
        .functions
        .iter()
        .any(|g| g.dispatch_receiver.is_none() && g.name == stub_name)
    {
        return false;
    }
    // An OVERLOADED top-level function (same name, multiple facade overloads) needs overload-aware routing
    // — the `$default` synthetics share the name `<name>$default` (distinct only by descriptor), and the
    // call site can't pick among them here. Skip (the omitted-default call falls back / skips).
    if ir
        .functions
        .iter()
        .filter(|g| g.dispatch_receiver.is_none() && g.name == f.name)
        .count()
        > 1
    {
        return false;
    }
    let n = f.params.len() as u32;
    let Some(defaults) = ir.param_defaults(fid) else {
        return false;
    };
    defaults
        .iter()
        .flatten()
        .all(|&d| default_expr_stub_safe(ir, d, n))
}

fn default_expr_stub_safe(ir: &IrFile, e: ExprId, n: u32) -> bool {
    match &ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= n => return false,
        IrExpr::SetValue { var, .. } if *var >= n => return false,
        IrExpr::Variable { index, .. } if *index >= n => return false,
        // A plain `new`/object construction (`f: F = F()`) is fine — the stub re-emits it. But a
        // VALUE/inline-class construction is NOT: it erases to its unboxed underlying (and mangles the
        // owning function's `$default` name), which the plain static stub doesn't box/unbox — so keep it
        // excluded (the file falls back to the inline call-site fill / skip).
        IrExpr::New { class, .. }
            if ir.classes.get(*class as usize).is_some_and(|c| c.is_value) =>
        {
            return false
        }
        IrExpr::NewExternal { internal, .. } | IrExpr::NewCrossFile { internal, .. }
            if ir.external_value_classes.contains_key(internal) =>
        {
            return false
        }
        // A closure (`Lambda`/`RefNew`) or an `invoke` reaches captured/spilled state the static stub
        // layout doesn't carry.
        IrExpr::Lambda { .. } | IrExpr::RefNew { .. } | IrExpr::InvokeFunction { .. } => {
            return false
        }
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name.contains('-') => return false,
        _ => {}
    }
    let mut ok = true;
    for_each_child(&ir.exprs, e, &mut |c| {
        if !default_expr_stub_safe(ir, c, n) {
            ok = false;
        }
    });
    ok
}

/// Shift every value index (`GetValue`/`SetValue`/`Variable`) `>= threshold` by `by`, throughout the
/// expression tree rooted at `e`. Used when a pass **appends parameters** to a function: the body's
/// locals (numbered from the old parameter count) must move up by the number of new parameters so
/// they don't collide with the inserted parameter slots.
pub fn shift_value_indices(ir: &mut IrFile, e: ExprId, threshold: u32, by: u32) {
    match &mut ir.exprs[e as usize] {
        IrExpr::GetValue(i) if *i >= threshold => *i += by,
        IrExpr::SetValue { var, .. } if *var >= threshold => *var += by,
        IrExpr::Variable { index, .. } if *index >= threshold => *index += by,
        // A `catch (e) { … }` variable is DECLARED by the `IrCatch.var` field (not a `Variable` node); its
        // uses inside the catch body are `GetValue`s shifted by the recursion below, so the field must
        // shift too or the binding and its reads desync.
        IrExpr::Try { catches, .. } => {
            for c in catches.iter_mut() {
                if c.var >= threshold {
                    c.var += by;
                }
            }
        }
        _ => {}
    }
    // A nested `Lambda`'s CAPTURES reference the ENCLOSING scope's value slots (shift them), but its
    // `inline_body` is a copy of the lambda's own body in the lambda's OWN value numbering (captures +
    // params) — recursing into it would corrupt those internal slots with this enclosing threshold/delta.
    // So for a `Lambda`, shift only the captures (the impl method's body is a separate function, already
    // untouched here).
    if let IrExpr::Lambda { captures, .. } = &ir.exprs[e as usize] {
        let caps = captures.clone();
        for c in caps {
            shift_value_indices(ir, c, threshold, by);
        }
        return;
    }
    let mut kids = Vec::new();
    for_each_child(&ir.exprs, e, &mut |c| kids.push(c));
    for c in kids {
        shift_value_indices(ir, c, threshold, by);
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
        let body = f.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let fun = f.add_fun(IrFunction {
            name: "answer".to_string(),
            params: vec![],
            ret: Ty::obj("kotlin/Int"),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(f.functions[fun as usize].name, "answer");
        // The return type is a Kotlin FqName, not a JVM descriptor — the backend maps it.
        match f.functions[fun as usize].ret.obj_internal() {
            Some(fq) => assert_eq!(fq, "kotlin/Int"),
            None => panic!("expected class type"),
        }
        assert!(matches!(f.expr(body), IrExpr::Block { .. }));
    }

    #[test]
    fn shift_value_indices_shifts_lambda_captures_not_inline_body() {
        // A `Lambda` whose CAPTURE references the enclosing slot 1 and whose `inline_body` references the
        // lambda's OWN slot 1. Shifting the enclosing scope (threshold 1, +2) must shift the capture
        // (1 → 3) but leave the lambda-internal `inline_body` reference (1) untouched.
        let mut f = IrFile::default();
        let cap = f.add_expr(IrExpr::GetValue(1)); // capture of enclosing value 1
        let inner = f.add_expr(IrExpr::GetValue(1)); // the lambda's OWN value 1
        let lam = f.add_expr(IrExpr::Lambda {
            impl_fn: 0,
            arity: 0,
            captures: vec![cap],
            sam: None,
            inline_body: Some(inner),
        });
        let outer = f.add_expr(IrExpr::GetValue(1)); // an enclosing value 1, sibling of the lambda
        let block = f.add_expr(IrExpr::Block {
            stmts: vec![lam],
            value: Some(outer),
        });
        shift_value_indices(&mut f, block, 1, 2);
        assert!(
            matches!(f.exprs[cap as usize], IrExpr::GetValue(3)),
            "capture must shift 1 -> 3"
        );
        assert!(
            matches!(f.exprs[outer as usize], IrExpr::GetValue(3)),
            "enclosing ref must shift 1 -> 3"
        );
        assert!(
            matches!(f.exprs[inner as usize], IrExpr::GetValue(1)),
            "lambda-internal inline_body ref must NOT shift"
        );
    }

    #[test]
    fn ir_field_new_uses_kotlin_defaults() {
        let f = IrField::new("x".to_string(), Ty::Int);
        assert_eq!(f.name, "x");
        assert_eq!(f.ty, Ty::Int);
        assert_eq!(f.type_param, None);
        assert_eq!(f.default, None);
        // Kotlin default: private backing field, not known-final, not lateinit.
        assert!(f.is_private);
        assert!(!f.is_final);
        assert!(!f.is_lateinit);
    }

    #[test]
    fn arena_builders_append_and_index() {
        let mut f = IrFile::default();
        let a = f.add_expr(IrExpr::Const(IrConst::Int(1)));
        let b = f.add_expr(IrExpr::Const(IrConst::Int(2)));
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert!(matches!(f.expr(a), IrExpr::Const(IrConst::Int(1))));

        let fid = f.add_fun(IrFunction {
            name: "g".to_string(),
            params: vec![],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(fid, 0);
        let cid = f.add_class(IrClass {
            fq_name: "demo/C".to_string(),
            ..blank_class("demo/C")
        });
        assert_eq!(cid, 0);
        assert_eq!(f.classes[cid as usize].fq_name, "demo/C");
    }

    #[test]
    fn for_each_child_visits_every_direct_operand() {
        let mut f = IrFile::default();
        let lhs = f.add_expr(IrExpr::Const(IrConst::Int(1)));
        let rhs = f.add_expr(IrExpr::Const(IrConst::Int(2)));
        let bin = f.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs,
            rhs,
        });
        let mut kids = Vec::new();
        for_each_child(&f.exprs, bin, &mut |c| kids.push(c));
        assert_eq!(kids, vec![lhs, rhs]);

        // A leaf node (Const) has no children.
        let mut none = Vec::new();
        for_each_child(&f.exprs, lhs, &mut |c| none.push(c));
        assert!(none.is_empty());

        // A block visits its statements then its value.
        let blk = f.add_expr(IrExpr::Block {
            stmts: vec![lhs, rhs],
            value: Some(bin),
        });
        let mut bk = Vec::new();
        for_each_child(&f.exprs, blk, &mut |c| bk.push(c));
        assert_eq!(bk, vec![lhs, rhs, bin]);
    }

    /// A minimal well-formed `IrClass` for tests that only exercise fields/functions on the file.
    fn blank_class(fq: &str) -> IrClass {
        IrClass {
            fq_name: fq.to_string(),
            is_value: false,
            type_param_bounds: Vec::new(),
            type_params: Vec::new(),
            supertypes: Vec::new(),
            fields: Vec::new(),
            serial_names: Vec::new(),
            custom_serializer: None,
            field_serializers: Vec::new(),
            contextual_fields: Vec::new(),
            ctor_param_count: 0,
            ctor_args: Vec::new(),
            init_body: None,
            explicit_param_stores: false,
            methods: Vec::new(),
            is_interface: false,
            is_annotation: false,
            annotation_impl_of: None,
            is_sealed: false,
            is_abstract: false,
            superclass: "kotlin/Any".to_string(),
            super_args: Vec::new(),
            enum_entries: Vec::new(),
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            bridges: Vec::new(),
            interfaces: Vec::new(),
            is_object: false,
            is_companion: false,
            companion_class: None,
            secondary_ctors: Vec::new(),
            has_primary_ctor: true,
            applied_annotations: Vec::new(),
            runtime_retained: false,
        }
    }

    fn add_toplevel_fn(f: &mut IrFile, name: &str, param: Ty) -> u32 {
        f.add_fun(IrFunction {
            name: name.to_string(),
            params: vec![param],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    #[test]
    fn toplevel_default_stub_safe_accepts_a_simple_constant_default() {
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "greet", Ty::Int);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(toplevel_default_stub_safe(&f, fid));
    }

    #[test]
    fn toplevel_default_stub_safe_rejects_mangled_and_missing_defaults() {
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "greet-abc123", Ty::Int);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(5)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(!toplevel_default_stub_safe(&f, fid));

        let mut g = IrFile::default();
        let gid = add_toplevel_fn(&mut g, "hello", Ty::Int);
        assert!(!toplevel_default_stub_safe(&g, gid));
    }

    #[test]
    fn toplevel_default_stub_safe_rejects_overloaded_and_unsafe_default() {
        let mut f = IrFile::default();
        let fid = add_toplevel_fn(&mut f, "over", Ty::Int);
        add_toplevel_fn(&mut f, "over", Ty::String);
        let def = f.add_expr(IrExpr::Const(IrConst::Int(0)));
        f.fn_params
            .insert(fid, FnParamInfo::defaults(Vec::new(), vec![Some(def)]));
        assert!(!toplevel_default_stub_safe(&f, fid));

        let mut g = IrFile::default();
        let gid = add_toplevel_fn(&mut g, "spill", Ty::Int);
        let bad = g.add_expr(IrExpr::GetValue(3));
        g.fn_params
            .insert(gid, FnParamInfo::defaults(Vec::new(), vec![Some(bad)]));
        assert!(!toplevel_default_stub_safe(&g, gid));
    }
}
