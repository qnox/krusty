//! Backend-neutral lifting provenance for the callables a file declares in executable code.
//!
//! kotlinc lifts every lambda and local function out of the body it was written in
//! (`LocalDeclarationsLowering`) and names the result after the declarations around it:
//! `outer$inner`, `outer$lambda$0`, `outer$inner$lambda$2`. The numbers come from ONE sequence per
//! lexical owner and outermost declaration name, which the lambdas and the local functions whose
//! name is already taken there share, in source order. Overloads share it, as do a class's
//! constructors and initializers (all `<init>`).
//!
//! This walk records where each lambda and local function sits in that sequence. Whether a lambda
//! takes a number depends on its type (a suspend lambda becomes a class of its own and is not
//! lifted), so the numbers themselves are assigned once checking has decided that.
//!
//! What the walk visits mirrors kotlinc's tree at that phase:
//! - A property's initializer is named after the property, its delegate after `<name>$delegate`
//!   and its accessors `<get-name>`/`<set-name>`.
//! - Constructor parameter defaults, superclass arguments, enum entry arguments, `init` blocks and
//!   secondary constructors are all `<init>`. An enum entry with a body is a class of its own for
//!   its members.
//! - A local delegated property's accessors are local functions without a source name: each takes
//!   a position after the delegate expression, like a lambda.
//! - A local class or an anonymous object starts sequences of its own.

use std::collections::HashMap;

use crate::ast::{
    ClassDecl, ClassInit, CtorDelegation, Decl, Expr, ExprId, File, FunBody, FunDecl, LiftingSite,
    LiftingStep, LocalDelegateProvenance, PropDecl, Stmt, StmtId,
};
use crate::lifting_provenance::LiftingCallableKind;

/// The counters of one file, carried across its declaration units.
#[derive(Default)]
pub(super) struct LiftingCounters {
    /// The next source position of each sequence.
    sequences: HashMap<(String, String), u32>,
    /// The next ordinal of each lexical owner's local delegated properties.
    delegated_locals: HashMap<String, u32>,
}

/// The sites one walk records for a file's lambdas and local functions.
#[derive(Default)]
pub(super) struct LiftingSites {
    pub lambdas: HashMap<u32, LiftingSite>,
    pub local_functions: HashMap<StmtId, LiftingSite>,
    pub local_delegates: HashMap<StmtId, LocalDelegateProvenance>,
}

/// One sequence and the local callables enclosing the walk's current position in it.
#[derive(Clone)]
struct Scope {
    owner: String,
    container: String,
    path: Vec<LiftingStep>,
}

impl Scope {
    fn new(owner: &str, container: impl Into<String>) -> Self {
        Self {
            owner: owner.to_string(),
            container: container.into(),
            path: Vec::new(),
        }
    }
}

enum Child {
    Expr(ExprId),
    Stmt(StmtId),
}

struct Walker<'a> {
    file: &'a File,
    counters: &'a mut LiftingCounters,
    sites: LiftingSites,
    /// Each local delegated property walked, with its lexical owner.
    local_delegate_owners: Vec<(StmtId, String)>,
}

/// Record the lifting site of every lambda and local function `file` declares.
pub(super) fn record(file: &File, counters: &mut LiftingCounters) -> LiftingSites {
    let mut walker = Walker {
        file,
        counters,
        sites: LiftingSites::default(),
        local_delegate_owners: Vec::new(),
    };
    let anonymous = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    for &declaration in &file.decls {
        if anonymous.contains(&declaration) || file.is_local_declaration(declaration) {
            continue;
        }
        match file.decl(declaration) {
            Decl::Fun(function) => walker.function(function, "file"),
            Decl::Property(property) => walker.property(property, "file"),
            Decl::Class(class) => walker.class_body(class, &format!("class:{}", class.name), false),
        }
    }
    walker.number_local_delegates();
    walker.sites
}

impl Walker<'_> {
    /// Number the local delegated properties of each lexical owner in source order, which kotlinc
    /// spells `<v#N>`. The walk itself visits a class's initializers before its functions, and the
    /// file's declaration units arrive in source order.
    fn number_local_delegates(&mut self) {
        let spans = &self.file.stmt_spans;
        let mut walked = std::mem::take(&mut self.local_delegate_owners);
        walked.sort_by_key(|(statement, _)| spans.get(statement.0 as usize).map(|span| span.lo));
        for (statement, owner) in walked {
            let next = self.counters.delegated_locals.entry(owner).or_insert(0);
            if let Some(provenance) = self.sites.local_delegates.get_mut(&statement) {
                provenance.ordinal = *next;
            }
            *next += 1;
        }
    }

    /// `scope` extended by the next position of its sequence.
    fn next(&mut self, scope: &Scope, kind: LiftingCallableKind, name: Option<&str>) -> Scope {
        let counter = self
            .counters
            .sequences
            .entry((scope.owner.clone(), scope.container.clone()))
            .or_insert(0);
        let position = *counter;
        *counter += 1;
        let mut path = scope.path.clone();
        path.push(LiftingStep {
            kind,
            name: name.map(str::to_string),
            position,
        });
        Scope {
            owner: scope.owner.clone(),
            container: scope.container.clone(),
            path,
        }
    }

    fn site(scope: &Scope) -> LiftingSite {
        LiftingSite {
            owner: scope.owner.clone(),
            container: scope.container.clone(),
            path: scope.path.clone(),
        }
    }

    fn function(&mut self, function: &FunDecl, owner: &str) {
        let scope = Scope::new(owner, function.name.as_str());
        self.function_body(function, &scope);
    }

    fn function_body(&mut self, function: &FunDecl, scope: &Scope) {
        for default in function
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
        {
            self.expr(default, scope);
        }
        self.body(&function.body, scope);
    }

    fn body(&mut self, body: &FunBody, scope: &Scope) {
        match body {
            FunBody::Expr(root) | FunBody::Block(root) => self.expr(*root, scope),
            FunBody::None => {}
        }
    }

    fn property(&mut self, property: &PropDecl, owner: &str) {
        if let Some(initializer) = property.init {
            self.expr(initializer, &Scope::new(owner, property.name.as_str()));
        }
        if let Some(delegate) = property.delegate {
            self.expr(
                delegate,
                &Scope::new(owner, format!("{}$delegate", property.name)),
            );
        }
        if let Some(getter) = &property.getter {
            self.body(
                getter,
                &Scope::new(owner, format!("<get-{}>", property.name)),
            );
        }
        if let Some(body) = property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
        {
            self.body(body, &Scope::new(owner, format!("<set-{}>", property.name)));
        }
    }

    /// Walk a classifier's members under `owner`. `anonymous` is set for an anonymous object, whose
    /// super-constructor arguments its construction site has already walked.
    fn class_body(&mut self, class: &ClassDecl, owner: &str, anonymous: bool) {
        let initializer = Scope::new(owner, "<init>");
        for default in class.props.iter().filter_map(|parameter| parameter.default) {
            self.expr(default, &initializer);
        }
        if !anonymous {
            for argument in &class.base_args {
                self.expr(*argument, &initializer);
            }
        }
        for (index, delegation) in class.interface_delegations.iter().enumerate() {
            self.expr(
                delegation.value,
                &Scope::new(owner, format!("$$delegate_{index}")),
            );
        }
        for entry in &class.enum_entries {
            let has_body = !entry.methods.is_empty()
                || !entry.props.is_empty()
                || !entry.init_order.is_empty();
            // kotlinc numbers the arguments of an entry with a body in the entry's own class, where
            // it places them. The arguments' code is emitted in the enum's static initializer here,
            // so they are numbered with the enum's, which keeps their methods distinct.
            for argument in &entry.args {
                self.expr(*argument, &initializer);
            }
            if has_body {
                let entry_owner = format!("{owner}.{}", entry.name);
                self.initializers(&entry.init_order, &entry.props, &entry_owner);
                for method in &entry.methods {
                    self.function(method, &entry_owner);
                }
            }
        }
        // `init` blocks and secondary constructors share `<init>`, in source order.
        let mut constructors = class.secondary_ctors.iter().collect::<Vec<_>>();
        constructors.sort_by_key(|constructor| constructor.span.lo);
        let mut constructors = constructors.into_iter().peekable();
        for step in &class.init_order {
            match step {
                ClassInit::Block(block) => {
                    let start = self.file.expr_spans[block.0 as usize].lo;
                    while let Some(constructor) =
                        constructors.next_if(|constructor| constructor.span.lo < start)
                    {
                        self.secondary_constructor(constructor, &initializer);
                    }
                    self.expr(*block, &initializer);
                }
                ClassInit::PropInit(index) => self.property(&class.body_props[*index], owner),
            }
        }
        for constructor in constructors {
            self.secondary_constructor(constructor, &initializer);
        }
        for method in &class.methods {
            self.function(method, owner);
        }
    }

    fn initializers(&mut self, order: &[ClassInit], properties: &[PropDecl], owner: &str) {
        let initializer = Scope::new(owner, "<init>");
        for step in order {
            match step {
                ClassInit::Block(block) => self.expr(*block, &initializer),
                ClassInit::PropInit(index) => self.property(&properties[*index], owner),
            }
        }
    }

    fn secondary_constructor(&mut self, constructor: &crate::ast::SecondaryCtor, scope: &Scope) {
        for default in constructor
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
        {
            self.expr(default, scope);
        }
        match &constructor.delegation {
            CtorDelegation::This(call) | CtorDelegation::Super(call) => {
                for argument in &call.args {
                    self.expr(*argument, scope);
                }
            }
            CtorDelegation::None => {}
        }
        if let Some(body) = constructor.body {
            self.expr(body, scope);
        }
    }

    fn expr(&mut self, expression: ExprId, scope: &Scope) {
        let file = self.file;
        match file.expr(expression) {
            Expr::Lambda { body, .. } => {
                let own = self.next(scope, LiftingCallableKind::Lambda, None);
                self.sites.lambdas.insert(expression.0, Self::site(&own));
                self.expr(*body, &own);
            }
            Expr::Call { .. } if file.anonymous_object_classes.contains_key(&expression) => {
                let declaration = file.anonymous_object_classes[&expression];
                let Decl::Class(class) = file.decl(declaration) else {
                    return;
                };
                for argument in &class.base_args {
                    self.expr(*argument, scope);
                }
                let owner = Self::local_owner("anonymous", file.expr_spans[expression.0 as usize]);
                self.class_body(class, &owner, true);
            }
            _ => {
                let children = std::cell::RefCell::new(Vec::new());
                file.any_child_expr(
                    expression,
                    &mut |child| {
                        children.borrow_mut().push(Child::Expr(child));
                        false
                    },
                    &mut |statement| {
                        children.borrow_mut().push(Child::Stmt(statement));
                        false
                    },
                );
                for child in children.into_inner() {
                    match child {
                        Child::Expr(child) => self.expr(child, scope),
                        Child::Stmt(statement) => self.stmt(statement, scope),
                    }
                }
            }
        }
    }

    /// The sequence owner of a local class or anonymous object, keyed by where the file writes it.
    /// A declaration id is only unique within its declaration unit, while the sequences span the
    /// whole file: two units' anonymous objects would otherwise share one owner and number on.
    fn local_owner(kind: &str, written: crate::diag::Span) -> String {
        format!("{kind}@{}", written.lo)
    }

    fn stmt(&mut self, statement: StmtId, scope: &Scope) {
        let file = self.file;
        match file.stmt(statement) {
            Stmt::LocalFun(function) => {
                let own = self.next(
                    scope,
                    LiftingCallableKind::LocalFunction,
                    Some(&function.name),
                );
                self.sites
                    .local_functions
                    .insert(statement, Self::site(&own));
                self.function_body(function, &own);
            }
            Stmt::LocalDelegate {
                is_var, delegate, ..
            } => {
                self.expr(*delegate, scope);
                // Its accessors are local functions without a source name.
                let accessors = if *is_var { 2 } else { 1 };
                let accessors = (0..accessors)
                    .map(|_| {
                        Self::site(&self.next(
                            scope,
                            LiftingCallableKind::LocalDelegatedPropertyAccessor,
                            None,
                        ))
                    })
                    .collect();
                // Numbered once the whole unit is walked: see `number_local_delegates`.
                let provenance = LocalDelegateProvenance {
                    accessors,
                    ordinal: 0,
                };
                self.sites.local_delegates.insert(statement, provenance);
                self.local_delegate_owners
                    .push((statement, scope.owner.clone()));
            }
            Stmt::LocalClass(_) => {
                if let Some(&declaration) = file.local_class_decls.get(&statement) {
                    if let Decl::Class(hoisted) = file.decl(declaration) {
                        let owner =
                            Self::local_owner("local", file.stmt_spans[statement.0 as usize]);
                        self.class_body(hoisted, &owner, false);
                    }
                }
            }
            _ => {
                let mut children = Vec::new();
                file.any_child_stmt(statement, &mut |child| {
                    children.push(child);
                    false
                });
                for child in children {
                    self.expr(child, scope);
                }
            }
        }
    }
}
