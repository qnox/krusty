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
    Class {
        fq_name: String,
        type_args: Vec<IrType>,
        nullable: bool,
    },
    /// A type-parameter reference (`T`), resolved to its declaration index.
    TypeParameter(u32),
    /// `(P..) -> R` function type — kept structural so backends choose the representation
    /// (JVM `FunctionN`, a JS closure, …).
    Function {
        params: Vec<IrType>,
        ret: Box<IrType>,
        /// A `suspend` function type — the JVM realizes it as `Function{n+1}` (a trailing
        /// `kotlin/coroutines/Continuation` parameter, `Object`-erased result).
        suspend: bool,
    },
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
    /// A top-level function defined in ANOTHER source file of the same multi-file compilation —
    /// `invokestatic <facade>.<name>(params)ret`. Carries the signature as backend-agnostic `IrType`s
    /// (the JVM backend builds the descriptor), so `ir_lower` needn't know JVM descriptors. Distinct
    /// from `Local` (same IrFile, by index) and `Static` (a resolved classpath/library method).
    CrossFile {
        facade: String,
        name: String,
        params: Vec<IrType>,
        ret: IrType,
    },
    /// An instance method (or property accessor) of a class defined in ANOTHER file of the same
    /// compilation — `invokevirtual`/`invokeinterface owner.name(params)ret` on the `dispatch_receiver`.
    /// Like `Virtual` but carries `IrType`s (the JVM backend builds the descriptor), so `ir_lower` needs
    /// no JVM descriptor for a sibling-file user class (resolved from its `ClassSig`).
    CrossFileVirtual {
        owner: String,
        name: String,
        params: Vec<IrType>,
        ret: IrType,
        interface: bool,
    },
    /// A resolved classpath static method — `invokestatic owner.name:descriptor`. Used for stdlib
    /// extension/top-level functions resolved from the classpath (`StringsKt.repeat`, `RangesKt.until`),
    /// carrying the exact JVM descriptor so no name is hardcoded in the backend.
    /// `inline` => the callee is a Kotlin `inline` function (set from the resolved signature's metadata);
    /// the JVM backend may splice its compiled body here instead of emitting the `invokestatic`.
    Static {
        owner: String,
        name: String,
        descriptor: String,
        inline: bool,
        /// True when the callee is a NON-PUBLIC `@InlineOnly` function (`require`/`check`/`error`): there
        /// is no legal `invokestatic` fallback, so the backend MUST splice the body. If it can't (a
        /// branchy body on a non-empty operand stack), the whole file is skipped — never miscompiled.
        must_inline: bool,
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
        type_operand: IrType,
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
        ty: IrType,
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
        ctor_params: Option<Vec<IrType>>,
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
    /// Invoke a function value (`f(args)` where `f: (A,…) -> R`) via the `FunctionN.invoke` interface
    /// method. Arguments are boxed to `Object`; the `Object` result is cast/unboxed to `ret`.
    InvokeFunction {
        func: ExprId,
        args: Vec<ExprId>,
        ret: IrType,
    },
    /// The not-null assertion `operand!!` — yields `operand`, throwing if it is null. On the JVM this
    /// is `kotlin/jvm/internal/Intrinsics.checkNotNull` applied to a duplicate of the value.
    NotNullAssert {
        operand: ExprId,
    },
    /// Construct an instance of a classpath (non-IR) class — `RuntimeException("x")`, `StringBuilder()`.
    /// `internal` is the JVM internal name, `ctor_desc` the `(…)V` constructor descriptor.
    NewExternal {
        internal: String,
        ctor_desc: String,
        args: Vec<ExprId>,
    },
    /// Construct a class defined in ANOTHER file of the same compilation — `new internal; dup; <args>;
    /// invokespecial internal.<init>(params)V`. Like `NewExternal` but carries the ctor parameter types
    /// as `IrType`s (the JVM backend builds the descriptor) since it's a sibling-file user class, not a
    /// classpath one with a library-provided descriptor.
    NewCrossFile {
        internal: String,
        params: Vec<IrType>,
        args: Vec<ExprId>,
    },
    /// A `kotlin/jvm/internal/Ref$XxxRef` holder boxing a mutable local that a closure captures: a
    /// new `Ref$IntRef`/`Ref$ObjectRef`/… whose `element` field is initialized to `init`. `elem` is
    /// the boxed value's type (selects the `Ref` subclass + the `element` field descriptor). Evaluates
    /// to the holder, so it's the initializer of the local that holds the box.
    RefNew {
        elem: IrType,
        init: ExprId,
    },
    /// Read a boxed mutable local: `holder.element` (`getfield Ref$XxxRef.element`).
    RefGet {
        holder: ExprId,
        elem: IrType,
    },
    /// Write a boxed mutable local: `holder.element = value` (`putfield`), evaluating to `value`.
    RefSet {
        holder: ExprId,
        elem: IrType,
        value: ExprId,
    },
    /// `throw operand` — throws the (Throwable) value; control never falls through (`Nothing`).
    Throw {
        operand: ExprId,
    },
    /// A `vararg` argument at a call site (Kotlin IR's `IrVararg`): the spread/listed elements and
    /// their element type. The JVM backend packs them into an array; another backend may differ.
    Vararg {
        element_type: IrType,
        elements: Vec<ExprId>,
    },
    /// Allocate an uninitialized array of `size` elements (`anewarray` for a reference element,
    /// `newarray` for a primitive) — the sized constructor `Array<T>(n) { … }` / `arrayOfNulls<T>(n)`
    /// fills it afterwards. (`Vararg` is the *literal* form with a statically-known element list.)
    NewArray {
        element_type: IrType,
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
        result: IrType,
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
    /// `@JvmInline value class` — a single-field class represented unboxed (as its one field's type) by
    /// the JVM `jvm::value_classes` IR pass. The IR otherwise treats it as a plain class.
    pub is_value: bool,
    /// Declared non-`Any` generic upper bounds (`<T: String>` → `("T", String)`), carried verbatim from
    /// the source. Platform-neutral metadata; the JVM value-class pass uses it to erase a value class's
    /// underlying type parameter to its bound (`value class S<T: String>` → `String`).
    pub type_param_bounds: Vec<(String, IrType)>,
    /// Parallel to `fields`: the source type-parameter NAME a field was declared with (`val x: T` →
    /// `Some("T")`), else `None` for a concrete type. Platform-neutral; lets the value-class pass pick the
    /// CORRECT bound for a generic underlying (vs guessing), independent of erasure dropping the name.
    pub field_type_params: Vec<Option<String>>,
    pub supertypes: Vec<IrType>,
    /// Instance fields `(name, type)`. The first `ctor_param_count` are the primary-constructor
    /// parameters (stored directly from args, in order); any after them are class-body properties
    /// initialized by `init_body`.
    pub fields: Vec<(String, IrType)>,
    /// How many leading `fields` are property constructor parameters (`val`/`var`) — the rest are body
    /// properties. NOTE: this is the count of constructor params that BACK A FIELD, not the total
    /// constructor arity (a non-`val`/`var` parameter is an argument only, no field) — see `ctor_args`.
    pub ctor_param_count: u32,
    /// ALL primary-constructor parameters in declaration order: `(type, is_field)`. `is_field` ⇒ a
    /// `val`/`var` property whose arg is stored to a field (the property fields are `fields[0..]` in the
    /// same relative order); `!is_field` ⇒ a plain parameter, an argument only, available as a local in
    /// `<init>` for property initializers / `init` blocks. Empty for synthesized/enum/object classes
    /// (then the constructor arity is `ctor_param_count`).
    pub ctor_args: Vec<(IrType, bool)>,
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
    /// Parallel to `enum_entries`: `Some(subclass_fq)` when that entry has a body and is constructed
    /// as an instance of a synthesized anonymous subclass (`new Enum$ENTRY(name, ordinal, args)`),
    /// else `None` (constructed as the enum itself). Non-empty only on an `enum class` with bodies.
    pub enum_entry_subclass: Vec<Option<String>>,
    /// `Some(user_field_types)` marks this class as a synthesized enum-entry subclass: it extends the
    /// enum (`superclass`), has no own fields, and its constructor is `(String name, int ordinal,
    /// <user_field_types>)V` delegating to the enum's `(String,int,<user>)V` constructor.
    pub enum_entry_of: Option<Vec<IrType>>,
    /// `Some(..)` marks this class as a synthesized property-reference singleton: a `final class
    /// extends kotlin/jvm/internal/PropertyReference1Impl` (the `superclass`) with a `public static
    /// final INSTANCE`, a constructor `super(owner.class, name, signature, 0)`, and a `get(Object)
    /// Object` override that reads the referenced property via its getter.
    pub prop_ref: Option<PropRef>,
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
    /// Per-primary-constructor-parameter `Some(name)` when the backend should guard it with a non-null
    /// assertion (`Intrinsics.checkNotNullParameter`) at `<init>` entry — a non-null reference param.
    /// Parallel to the first `ctor_param_count` `fields`. Empty for synthesized/enum/object classes.
    pub ctor_param_checks: Vec<Option<String>>,
    /// `true` for a synthesized `C$Companion` class: a private no-arg constructor and no own singleton
    /// field (the `Companion` instance is held by the outer class).
    pub is_companion: bool,
    /// `Some(companion_fq)` on a class with a `companion object`: emit a `public static final
    /// <companion> Companion` field, initialized in this class's `<clinit>`.
    pub companion_class: Option<String>,
    /// Per-field `true` when the backing field is immutable (`val`) — emitted `final`. Parallel to
    /// `fields` (empty ⇒ none final, for synthesized classes).
    pub field_final: Vec<bool>,
    /// Per-field visibility (platform-neutral): `true` = `private` (the default — Kotlin backing fields
    /// are private, reached via accessors); `false` = non-private, readable/writable cross-class (a
    /// coroutine continuation's `result`/`label`). Parallel to `fields`; empty ⇒ all private. Each
    /// backend maps this to its own access representation (the JVM emitter → `ACC_PRIVATE`/`ACC_PUBLIC`).
    pub field_private: Vec<bool>,
    /// Secondary constructors — each an extra `<init>(params)` that delegates to the primary
    /// constructor (`constructor(…) : this(args)`) then runs its body. Empty for most classes.
    pub secondary_ctors: Vec<IrSecondaryCtor>,
    /// `false` for a class with NO primary constructor: the backend emits no primary `<init>`; every
    /// `<init>` comes from `secondary_ctors` (a `Super`-delegating one carries the init body). `true`
    /// for every other class (including synthesized/enum/object).
    pub has_primary_ctor: bool,
}

/// A synthesized property-reference class's metadata (`Type::prop` → `Type$prop$N`): the referenced
/// property's owner, name, getter, and value type. The backend emits the `PropertyReference1Impl`
/// subclass from this — the `super(owner.class, name, "getName()desc", 0)` constructor and the
/// `get(Object)` override that reads `((Owner) it).getName()` (boxing a primitive result).
#[derive(Clone, Debug)]
pub struct PropRef {
    pub owner_internal: String,
    pub prop_name: String,
    pub getter_name: String,
    pub prop_ty: IrType,
    /// `false` = an unbound `Type::prop` (a `PropertyReference1Impl` singleton with `get(Object)`);
    /// `true` = a bound `obj::prop` (a `PropertyReference0Impl` constructed with the captured receiver,
    /// whose `get()` reads `this.receiver`).
    pub bound: bool,
}

/// A secondary constructor: `<init>(params)` evaluates `delegate_args`, calls the delegate target
/// (`invokespecial`), then runs `body`. `this` is value 0 and the parameters are values
/// `1..=params.len()` in `delegate_args`/`body`.
#[derive(Clone, Debug)]
pub struct IrSecondaryCtor {
    pub params: Vec<IrType>,
    pub delegate_args: Vec<ExprId>,
    pub body: Option<ExprId>,
    /// Which `<init>` this constructor delegates to, and whether it runs the class init body.
    pub delegate: CtorDelegateTarget,
}

/// The delegation target of a secondary constructor.
#[derive(Clone, Debug)]
pub enum CtorDelegateTarget {
    /// `this(args)` → `invokespecial` an own `<init>(target_params)` (the primary, or a sibling
    /// secondary in a no-primary class). The class init body runs in the reached constructor, not here.
    This { target_params: Vec<IrType> },
    /// `super(args)` (or implicit) in a class with NO primary constructor → `invokespecial` the
    /// superclass `<init>` (its signature is read live from the base class at emit time), then run the
    /// class init body (field initializers + `init {}`) before this constructor's own `body`.
    Super,
}

/// A synthetic bridge method (`name(erased_params)erased_ret` → `name(concrete_params)concrete_ret`).
#[derive(Clone, Debug)]
pub struct Bridge {
    pub name: String,
    pub erased_params: Vec<IrType>,
    pub erased_ret: IrType,
    pub concrete_params: Vec<IrType>,
    pub concrete_ret: IrType,
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
    pub ty: IrType,
    /// The initializer expression (run in `<clinit>` in declaration order).
    pub init: ExprId,
    /// `var` (mutable) ⇒ a setter is emitted and the backing field is non-`final`.
    pub is_var: bool,
    /// `const val` ⇒ kotlinc keeps the field `public static final` (inlined at use) with no accessor;
    /// a plain top-level `val`/`var` is `private static [final]` + a `public static` getter/setter.
    pub is_const: bool,
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
    /// `FunId` → each parameter's default-value expression (`None` = required). The *meaning* of a
    /// default is backend-agnostic language data; a backend chooses how to realize it (the JVM emits a
    /// `name$default(params, mask, marker)` stub; JS uses native default parameters).
    pub fn_param_defaults: std::collections::HashMap<u32, Vec<Option<ExprId>>>,
    /// `FunId` → parameter names, for mapping a call's named/omitted arguments onto positions. Recorded
    /// only for functions that have defaults (where such mapping is needed).
    pub fn_param_names: std::collections::HashMap<u32, Vec<String>>,
    /// Instance methods kotlinc leaves NON-`final` even in a final class — currently the data-class
    /// `Object`-overrides (`toString`/`hashCode`/`equals`), which kotlinc emits `public` (open) rather
    /// than `public final`. The JVM backend omits `ACC_FINAL` for a `FunId` in this set.
    pub open_methods: std::collections::HashSet<u32>,
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
    pub suspend_calls: std::collections::HashMap<u32, IrType>,
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
        let body = f.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let fun = f.add_fun(IrFunction {
            name: "answer".to_string(),
            params: vec![],
            ret: IrType::Class {
                fq_name: "kotlin/Int".to_string(),
                type_args: vec![],
                nullable: false,
            },
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
