//! kotlinc's order for the values a local class captures.
//!
//! The constructor parameters and fields of a local class follow the order in which kotlinc's
//! closure analysis first meets each captured value. A closure lists the values its own code reads,
//! in source order, and then the values of each closure nested in it, in declaration order. For a
//! class, its own code is the property initializers, `init` blocks and delegate values; its nested
//! closures are the primary constructor (parameter defaults, then super-constructor arguments)
//! followed by the members. Lambdas, local functions, and local or anonymous classes nest the same
//! way inside any of those bodies, inline-call lambdas included.

use std::cell::RefCell;

use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, Expr, ExprId, File, FunBody, Stmt, StmtId,
};
use crate::fx_hash::HashSet;

/// Every enclosing-value spelling the class reads, in kotlinc's capture order.
pub(in crate::resolve) fn class_capture_order(file: &File, class: &ClassDecl) -> Vec<String> {
    let mut order = Vec::new();
    collect(file, Closure::Class(class), &HashSet::default(), &mut order);
    order
}

enum Closure<'f> {
    Class(&'f ClassDecl),
    /// A callable body: each read runs with the names bound before it.
    Callable(Vec<Piece>),
}

enum Piece {
    Read(ExprId),
    Bind(String),
}

struct Nested<'f> {
    position: u32,
    closure: Closure<'f>,
    bound: HashSet<String>,
}

struct Walk<'f> {
    file: &'f File,
    direct: Vec<String>,
    nested: Vec<Nested<'f>>,
}

fn collect(file: &File, closure: Closure<'_>, bound: &HashSet<String>, order: &mut Vec<String>) {
    let mut walk = Walk {
        file,
        direct: Vec::new(),
        nested: Vec::new(),
    };
    match closure {
        Closure::Callable(pieces) => walk.callable(&pieces, bound),
        Closure::Class(class) => walk.class(class, bound),
    }
    walk.nested.sort_by_key(|nested| nested.position);
    for name in walk.direct {
        if !order.contains(&name) {
            order.push(name);
        }
    }
    for nested in walk.nested {
        collect(file, nested.closure, &nested.bound, order);
    }
}

fn function_pieces<'p>(
    params: impl IntoIterator<Item = (&'p str, Option<ExprId>)>,
    after: impl IntoIterator<Item = ExprId>,
) -> Vec<Piece> {
    let mut pieces = Vec::new();
    for (name, default) in params {
        pieces.extend(default.map(Piece::Read));
        pieces.push(Piece::Bind(name.to_string()));
    }
    pieces.extend(after.into_iter().map(Piece::Read));
    pieces
}

fn body_expression(body: &FunBody) -> Option<ExprId> {
    match body {
        FunBody::Expr(body) | FunBody::Block(body) => Some(*body),
        FunBody::None => None,
    }
}

impl<'f> Walk<'f> {
    fn defer(&mut self, position: u32, closure: Closure<'f>, bound: &HashSet<String>) {
        self.nested.push(Nested {
            position,
            closure,
            bound: bound.clone(),
        });
    }

    fn see(&mut self, name: &str, bound: &HashSet<String>) {
        if !bound.contains(name) && !self.direct.iter().any(|seen| seen == name) {
            self.direct.push(name.to_string());
        }
    }

    fn callable(&mut self, pieces: &[Piece], bound: &HashSet<String>) {
        let mut bound = bound.clone();
        for piece in pieces {
            match piece {
                Piece::Read(expression) => self.expression(*expression, &bound),
                Piece::Bind(name) => {
                    bound.insert(name.clone());
                }
            }
        }
    }

    fn class(&mut self, class: &'f ClassDecl, bound: &HashSet<String>) {
        for step in &class.init_order {
            match step {
                ClassInit::PropInit(index) => {
                    let property = &class.body_props[*index];
                    for expression in property.init.into_iter().chain(property.delegate) {
                        self.expression(expression, bound);
                    }
                }
                ClassInit::Block(body) => self.expression(*body, bound),
            }
        }
        for delegation in &class.interface_delegations {
            self.expression(delegation.value, bound);
        }
        // The primary constructor is the class's first declaration.
        let primary = function_pieces(
            class
                .props
                .iter()
                .map(|property| (property.name.as_str(), property.default)),
            class.base_args.iter().copied(),
        );
        self.defer(0, Closure::Callable(primary), bound);
        for method in &class.methods {
            let pieces = function_pieces(
                method
                    .params
                    .iter()
                    .map(|parameter| (parameter.name.as_str(), parameter.default)),
                body_expression(&method.body),
            );
            self.defer(method.span.lo, Closure::Callable(pieces), bound);
        }
        for property in &class.body_props {
            if let Some(body) = property.getter.as_ref().and_then(body_expression) {
                let position = self.position(body);
                self.defer(position, Closure::Callable(vec![Piece::Read(body)]), bound);
            }
            if let Some(setter) = &property.setter {
                if let Some(body) = setter.body.as_ref().and_then(body_expression) {
                    let parameter = setter.param.as_deref().unwrap_or("value");
                    let pieces = function_pieces([(parameter, None)], [body]);
                    self.defer(setter.span.lo, Closure::Callable(pieces), bound);
                }
            }
        }
        for constructor in &class.secondary_ctors {
            let delegation = match &constructor.delegation {
                CtorDelegation::None => &[][..],
                CtorDelegation::This(call) | CtorDelegation::Super(call) => call.args.as_slice(),
            };
            let pieces = function_pieces(
                constructor
                    .params
                    .iter()
                    .map(|parameter| (parameter.name.as_str(), parameter.default)),
                delegation.iter().copied().chain(constructor.body),
            );
            self.defer(constructor.span.lo, Closure::Callable(pieces), bound);
        }
    }

    fn position(&self, expression: ExprId) -> u32 {
        self.file.expr_span(expression).map_or(0, |span| span.lo)
    }

    fn expression(&mut self, expression: ExprId, bound: &HashSet<String>) {
        let file = self.file;
        if let Some(&declaration) = file.anonymous_object_classes.get(&expression) {
            if let Decl::Class(class) = file.decl(declaration) {
                let position = self.position(expression);
                self.defer(position, Closure::Class(class), bound);
            }
            return;
        }
        match file.expr(expression) {
            Expr::Name(name) => self.see(name, bound),
            Expr::Lambda { params, body } => {
                let mut pieces = params
                    .iter()
                    .map(|parameter| Piece::Bind(parameter.clone()))
                    .collect::<Vec<_>>();
                if params.is_empty() {
                    pieces.push(Piece::Bind("it".to_string()));
                }
                pieces.push(Piece::Read(*body));
                let position = self.position(expression);
                self.defer(position, Closure::Callable(pieces), bound);
            }
            Expr::Block { stmts, trailing } => {
                let mut bound = bound.clone();
                for &statement in stmts {
                    self.statement(statement, &mut bound);
                }
                if let Some(trailing) = trailing {
                    self.expression(*trailing, &bound);
                }
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                self.expression(*body, bound);
                for catch in catches {
                    let mut catch_bound = bound.clone();
                    catch_bound.insert(catch.name.clone());
                    self.expression(catch.body, &catch_bound);
                }
                if let Some(finally) = finally {
                    self.expression(*finally, bound);
                }
            }
            _ => {
                let children = RefCell::new(Vec::new());
                file.any_child_expr(
                    expression,
                    &mut |child| {
                        children.borrow_mut().push(Ok(child));
                        false
                    },
                    &mut |statement| {
                        children.borrow_mut().push(Err(statement));
                        false
                    },
                );
                for child in children.into_inner() {
                    match child {
                        Ok(child) => self.expression(child, bound),
                        Err(statement) => self.statement(statement, &mut bound.clone()),
                    }
                }
            }
        }
    }

    fn statement(&mut self, statement: StmtId, bound: &mut HashSet<String>) {
        let file = self.file;
        let position = file.stmt_spans[statement.0 as usize].lo;
        match file.stmt(statement) {
            Stmt::Assign { name, value } => {
                self.see(name, bound);
                self.expression(*value, bound);
            }
            Stmt::IncDec { name, .. } => self.see(name, bound),
            Stmt::Local { name, init, .. } => {
                self.expression(*init, bound);
                bound.insert(name.clone());
            }
            Stmt::LocalDelegate { name, delegate, .. } => {
                self.expression(*delegate, bound);
                bound.insert(name.clone());
            }
            Stmt::Destructure { entries, init } => {
                self.expression(*init, bound);
                bound.extend(entries.iter().map(|entry| entry.name.clone()));
            }
            Stmt::For {
                name, range, body, ..
            } => {
                self.expression(range.start, bound);
                self.expression(range.end, bound);
                let mut body_bound = bound.clone();
                body_bound.insert(name.clone());
                self.expression(*body, &body_bound);
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                self.expression(*iterable, bound);
                let mut body_bound = bound.clone();
                body_bound.insert(name.clone());
                self.expression(*body, &body_bound);
            }
            Stmt::LocalFun(function) => {
                let pieces = function_pieces(
                    function
                        .params
                        .iter()
                        .map(|parameter| (parameter.name.as_str(), parameter.default)),
                    body_expression(&function.body),
                );
                self.defer(position, Closure::Callable(pieces), bound);
            }
            Stmt::LocalClass(_) => {
                if let Some(Decl::Class(class)) = file
                    .local_class_decls
                    .get(&statement)
                    .map(|&declaration| file.decl(declaration))
                {
                    self.defer(position, Closure::Class(class), bound);
                }
            }
            _ => {
                let mut children = Vec::new();
                file.any_child_stmt(statement, &mut |child| {
                    children.push(child);
                    false
                });
                for child in children {
                    self.expression(child, bound);
                }
            }
        }
    }
}
