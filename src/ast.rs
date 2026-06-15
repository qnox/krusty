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
    Add, Sub, Mul, Div, Rem,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64),
    LongLit(i64),
    DoubleLit(f64),
    FloatLit(f32),
    BoolLit(bool),
    StringLit(String),
    CharLit(char),
    NullLit,
    Name(String),
    /// `operand!!` — not-null assertion (throws NPE if null, else the value).
    NotNull { operand: ExprId },
    /// `lhs ?: rhs` — Elvis (lhs if non-null, else rhs).
    Elvis { lhs: ExprId, rhs: ExprId },
    /// A string template `"a${e}b$c"` — alternating literal and interpolated-expression parts.
    Template(Vec<TemplatePart>),
    /// `receiver?.name` (args `None`) or `receiver?.name(args)` — a safe call: evaluates to `null`
    /// when the receiver is null, else the member access / call result.
    SafeCall { receiver: ExprId, name: String, args: Option<Vec<ExprId>> },
    /// `throw operand` — raises an exception; an expression of bottom type `Nothing`.
    Throw { operand: ExprId },
    /// A lambda literal `{ param -> body }` / `{ body }` (implicit `it`). krusty only supports it as
    /// the trailing argument of an *inlined* scope function (`let`/`also`); `body` is a `Block`.
    Lambda { param: Option<String>, body: ExprId },
    /// `try { body } catch (e: T) { … } … [finally { … }]` — the value is the body's, or a matching
    /// catch's; `finally` runs on every exit (for effect). Each `body`/handler/finally is a `Block`.
    Try { body: ExprId, catches: Vec<CatchClause>, finally: Option<ExprId> },
    /// `operand is T` / `operand !is T` — a type test (`instanceof`), evaluates to `Boolean`.
    Is { operand: ExprId, ty: TypeRef, negated: bool },
    /// `operand as T` / `operand as? T` — a cast (`checkcast`). `nullable` ⇒ `as?` (instanceof,
    /// `null` on mismatch). Result type is `T`.
    As { operand: ExprId, ty: TypeRef, nullable: bool },
    Unary { op: UnOp, operand: ExprId },
    Binary { op: BinOp, lhs: ExprId, rhs: ExprId },
    /// `receiver.name` (no call). For a bare name use `Name`.
    Member { receiver: ExprId, name: String },
    /// `array[index]` — array element read.
    Index { array: ExprId, index: ExprId },
    /// `callee(args)`. `callee` is `Name` (free function) or `Member` (method).
    Call { callee: ExprId, args: Vec<ExprId> },
    If { cond: ExprId, then_branch: ExprId, else_branch: Option<ExprId> },
    /// `{ stmts; trailing? }` — block as an expression; trailing expr is its value.
    Block { stmts: Vec<StmtId>, trailing: Option<ExprId> },
    /// `when (subject?) { conditions -> body ; else -> body }`. An arm with empty `conditions` is
    /// the `else`. With a subject, each condition is a value matched by `==`; without, each is a
    /// boolean expression.
    When { subject: Option<ExprId>, arms: Vec<WhenArm> },
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
    Local { is_var: bool, name: String, ty: Option<TypeRef>, init: ExprId },
    /// `name = value`
    Assign { name: String, value: ExprId },
    /// `receiver.name = value` — write a (mutable) property via its setter.
    AssignMember { receiver: ExprId, name: String, value: ExprId },
    /// `array[index] = value` — array element store.
    AssignIndex { array: ExprId, index: ExprId, value: ExprId },
    Return(Option<ExprId>),
    /// `break` / `continue` — loop control (unlabeled).
    Break,
    Continue,
    While { cond: ExprId, body: ExprId }, // body is a Block expr
    /// `for (name in start <op> end (step s)?) body` over an integer range.
    For { name: String, range: ForRange, body: ExprId },
    /// `for (name in iterable) body` over an array (element iteration).
    ForEach { name: String, iterable: ExprId, body: ExprId },
    Expr(ExprId),
    /// A local function declaration: `fun name(params): Ret { body }` inside a function body.
    /// Emitted as a private static method on the file/class with a mangled name.
    LocalFun(FunDecl),
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
    pub step: Option<ExprId>,
}

/// A syntactic type reference. v0: just a simple name (`Int`, `String`, ...).
#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: String,
    /// Trailing `?` — a nullable type (e.g. `String?`).
    pub nullable: bool,
    /// The first generic type argument, captured for `Array<T>` (other type args are erased/skipped).
    pub arg: Option<Box<TypeRef>>,
    pub span: Span,
    /// For function types `(A, B) -> R`: the parameter types. Empty for non-function types.
    /// When non-empty, `name` is `"<fun>"` and `arg` holds the return type.
    pub fun_params: Vec<TypeRef>,
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
    pub params: Vec<Param>,
    pub ret: Option<TypeRef>,
    pub body: FunBody,
    /// Generic type-parameter names (`fun <T, U> …`), erased to `Any`/`Object`.
    pub type_params: Vec<String>,
    pub span: Span,
    pub is_inline: bool,
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
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub name: String,
    /// Generic type-parameter names (`class C<T>`), erased to `Any`/`Object`.
    pub type_params: Vec<String>,
    pub props: Vec<PropParam>,
    /// Member functions declared in the class body (instance methods). v0: no secondary ctors.
    pub methods: Vec<FunDecl>,
    /// `companion object { … }` member functions — emitted as `static` methods on this class and
    /// called as `ClassName.fn(...)`.
    pub companion_methods: Vec<FunDecl>,
    /// `companion object { … }` properties (`const val`/`val`) — emitted as `static final` fields and
    /// read as `ClassName.PROP`.
    pub companion_props: Vec<PropDecl>,
    /// Properties declared in the class *body* (`class C { val x = … }`) — backing field + accessor,
    /// initialized in the primary constructor.
    pub body_props: Vec<PropDecl>,
    /// Constructor init steps in source order: a body-property initializer (index into `body_props`)
    /// or an `init { … }` block.
    pub init_order: Vec<ClassInit>,
    /// `data class` — synthesizes equals/hashCode/toString/componentN/copy.
    pub is_data: bool,
    /// `object Name { … }` — a singleton (one `INSTANCE`, private constructor).
    pub is_object: bool,
    /// `enum class Name { A, B }` — `enum_entries` lists the entry names (extends `java/lang/Enum`).
    pub is_enum: bool,
    pub enum_entries: Vec<String>,
    /// Constructor arguments per enum entry (parallel to `enum_entries`; empty for `A` with no args).
    /// The enum's primary-constructor parameters are in `props`.
    pub enum_entry_args: Vec<Vec<ExprId>>,
    /// `interface Name { … }` — a JVM interface (abstract methods).
    pub is_interface: bool,
    /// `open`/`abstract` — the class is not `final` (may be subclassed); `abstract` also adds
    /// `ACC_ABSTRACT`.
    pub is_open: bool,
    pub is_abstract: bool,
    /// `sealed` — abstract + open, and its subclasses are all known in this module (enabling
    /// exhaustive `when` without `else`).
    pub is_sealed: bool,
    /// Implemented interface names from a supertype list (`class C : I1, I2`).
    pub supertypes: Vec<String>,
    /// A base-class supertype `: Base(args)` (name + constructor arguments), if any.
    pub base_class: Option<String>,
    pub base_args: Vec<ExprId>,
    pub span: Span,
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
    pub ty: Option<TypeRef>,
    pub is_var: bool,
    /// `None` for a `lateinit var` (declared without an initializer; the backing field defaults to
    /// null and is assigned later).
    pub init: Option<ExprId>,
    /// `true` if declared `lateinit` — a no-initializer property is only allowed when lateinit
    /// (otherwise it's an abstract/interface property, which krusty rejects).
    pub is_lateinit: bool,
    /// A custom getter body (`val x: T get() = expr`/`get() { … }`) — a computed property with no
    /// backing field.
    pub getter: Option<FunBody>,
    pub span: Span,
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
    /// `typealias Name = Target` — maps alias simple name → target simple name.
    /// Generic type aliases are stored with the raw target name (type args erased).
    pub type_aliases: Vec<(String, String)>,
}

impl File {
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
                out.push_str(&format!("({} {}", if p.is_var { "var" } else { "val" }, p.name));
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
            Decl::Class(c) if c.is_interface => {
                out.push_str(&format!("(interface {}", c.name));
                for m in &c.methods {
                    out.push_str(&format!(" (absfun {})", m.name));
                }
                out.push(')');
            }
            Decl::Class(c) if c.is_enum => {
                out.push_str(&format!("(enum {}", c.name));
                for e in &c.enum_entries {
                    out.push_str(&format!(" {e}"));
                }
                out.push(')');
            }
            Decl::Class(c) => {
                out.push_str(&format!("({} {}", if c.is_object { "object" } else { "class" }, c.name));
                for p in &c.props {
                    out.push_str(&format!(" ({} {} {})", if p.is_var { "var" } else { "val" }, p.name, p.ty.name));
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
            Expr::Lambda { param, body } => {
                out.push_str(&format!("(lambda {} ", param.as_deref().unwrap_or("it")));
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
            Expr::Try { body, catches, finally } => {
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
            Expr::Is { operand, ty, negated } => {
                out.push_str(if *negated { "(!is " } else { "(is " });
                self.write_expr(*operand, out);
                out.push_str(&format!(" {})", ty.name));
            }
            Expr::As { operand, ty, nullable } => {
                out.push_str(if *nullable { "(as? " } else { "(as " });
                self.write_expr(*operand, out);
                out.push_str(&format!(" {})", ty.name));
            }
            Expr::SafeCall { receiver, name, args } => {
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
            Expr::If { cond, then_branch, else_branch } => {
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
        }
    }

    fn write_stmt(&self, id: StmtId, out: &mut String) {
        match self.stmt(id) {
            Stmt::Local { is_var, name, init, .. } => {
                out.push_str(&format!("({} {name} ", if *is_var { "var" } else { "val" }));
                self.write_expr(*init, out);
                out.push(')');
            }
            Stmt::Assign { name, value } => {
                out.push_str(&format!("(set {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::AssignMember { receiver, name, value } => {
                out.push_str("(set-member ");
                self.write_expr(*receiver, out);
                out.push_str(&format!(" {name} "));
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::AssignIndex { array, index, value } => {
                out.push_str("(set-index ");
                self.write_expr(*array, out);
                out.push(' ');
                self.write_expr(*index, out);
                out.push(' ');
                self.write_expr(*value, out);
                out.push(')');
            }
            Stmt::Break => out.push_str("(break)"),
            Stmt::Continue => out.push_str("(continue)"),
            Stmt::Return(e) => {
                out.push_str("(return");
                if let Some(e) = e {
                    out.push(' ');
                    self.write_expr(*e, out);
                }
                out.push(')');
            }
            Stmt::While { cond, body } => {
                out.push_str("(while ");
                self.write_expr(*cond, out);
                out.push(' ');
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::For { name, range, body } => {
                let op = match range.kind {
                    crate::ast::RangeKind::Through => "..",
                    crate::ast::RangeKind::Until => "until",
                    crate::ast::RangeKind::DownTo => "downTo",
                };
                out.push_str(&format!("(for {name} ("));
                self.write_expr(range.start, out);
                out.push_str(&format!(" {op} "));
                self.write_expr(range.end, out);
                if let Some(s) = range.step {
                    out.push_str(" step ");
                    self.write_expr(s, out);
                }
                out.push_str(") ");
                self.write_expr(*body, out);
                out.push(')');
            }
            Stmt::ForEach { name, iterable, body } => {
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
        }
    }
}

fn binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*", BinOp::Div => "/", BinOp::Rem => "%",
        BinOp::Eq => "==", BinOp::Ne => "!=", BinOp::Lt => "<", BinOp::Le => "<=",
        BinOp::Gt => ">", BinOp::Ge => ">=", BinOp::And => "&&", BinOp::Or => "||",
    }
}
fn unop(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
    }
}
