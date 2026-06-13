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
    BoolLit(bool),
    StringLit(String),
    Name(String),
    Unary { op: UnOp, operand: ExprId },
    Binary { op: BinOp, lhs: ExprId, rhs: ExprId },
    /// `receiver.name` (no call). For a bare name use `Name`.
    Member { receiver: ExprId, name: String },
    /// `callee(args)`. `callee` is `Name` (free function) or `Member` (method).
    Call { callee: ExprId, args: Vec<ExprId> },
    If { cond: ExprId, then_branch: ExprId, else_branch: Option<ExprId> },
    /// `{ stmts; trailing? }` — block as an expression; trailing expr is its value.
    Block { stmts: Vec<StmtId>, trailing: Option<ExprId> },
}

#[derive(Clone, Debug)]
pub enum Stmt {
    /// `val`/`var name (: type)? = init`
    Local { is_var: bool, name: String, ty: Option<TypeRef>, init: ExprId },
    /// `name = value`
    Assign { name: String, value: ExprId },
    Return(Option<ExprId>),
    While { cond: ExprId, body: ExprId }, // body is a Block expr
    Expr(ExprId),
}

/// A syntactic type reference. v0: just a simple name (`Int`, `String`, ...).
#[derive(Clone, Debug)]
pub struct TypeRef {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: TypeRef,
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
    pub span: Span,
}

/// A primary-constructor parameter that is also a property (`val`/`var name: Type`).
/// v0: property types are restricted to the primitive/String `Ty` set (no class-typed members yet).
#[derive(Clone, Debug)]
pub struct PropParam {
    pub name: String,
    pub ty: TypeRef,
    pub is_var: bool,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    pub name: String,
    pub props: Vec<PropParam>,
    /// Member functions declared in the class body (instance methods). v0: no secondary ctors.
    pub methods: Vec<FunDecl>,
    /// `data class` — synthesizes equals/hashCode/toString/componentN/copy.
    pub is_data: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Decl {
    Fun(FunDecl),
    Class(ClassDecl),
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
            Decl::Class(c) => {
                out.push_str(&format!("(class {}", c.name));
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
            Expr::BoolLit(b) => out.push_str(if *b { "true" } else { "false" }),
            Expr::StringLit(s) => out.push_str(&format!("{s:?}")),
            Expr::Name(n) => out.push_str(n),
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
            Stmt::Expr(e) => self.write_expr(*e, out),
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
