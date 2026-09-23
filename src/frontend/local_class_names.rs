//! kotlinc's names for the classes a file generates locally.
//!
//! kotlinc names every local class, anonymous object, lambda, callable reference and suspend
//! continuation in ONE walk over the file (`InventNamesForLocalClasses`), before any of them is
//! lowered: a lambda compiled through `invokedynamic` still takes its position in the sequence even
//! though no class is ever written for it. This module is that walk, over the source tree.
//!
//! A name is the chain of enclosing names joined by `$` — the file facade or classifier, then each
//! named declaration the node sits in (function, property, local variable, local function, local
//! class) — and, for a node without a name of its own, the next ordinal of the sequence the chain
//! numbers. Ordinals are counted per chain, case-insensitively (kotlinc keys them by the upper-cased
//! chain so two classes never differ only in case), and the count is shared by every node the chain
//! numbers: an anonymous object after two lambdas in `box` is `box$3`.
//!
//! What the walk visits mirrors kotlinc's tree at that phase rather than the source spelling alone:
//! - A suspend function reserves the first position of its own chain for its continuation, ahead of
//!   anything its body declares.
//! - A delegated property reserves one position before its delegate expression is walked, and each
//!   of its accessors then takes one: a member accessor through the property reference kotlinc
//!   passes to `getValue`/`setValue`, a local one as an accessor function without a source name.
//! - A constructor contributes no name, so what its parameters, delegation and `init` blocks declare
//!   is numbered in the class's own chain. An anonymous object's super-constructor arguments are
//!   numbered in the chain the object was written in, after the object itself.
//! - A field adds no name, so an interface delegate (`$$delegate_N`) is numbered in its class's
//!   chain.
//! - Temporaries (`for` iterators, destructuring containers, a local delegate's storage) contribute
//!   nothing.

use std::collections::HashMap;

use crate::ast::{
    AnonymousEnclosingFunction, ClassDecl, ClassInit, CtorDelegation, Decl, DeclId, Expr, ExprId,
    File, FunBody, FunDecl, PropDecl, Stmt, StmtId,
};

/// The names one walk invents for a file's local nodes.
#[derive(Default)]
pub(super) struct InventedLocalNames {
    /// Anonymous-object construction → its class declaration, invented name, and the source
    /// function that lexically encloses it.
    pub anonymous_objects: Vec<(ExprId, DeclId, String, Option<AnonymousEnclosingFunction>)>,
    /// Suspend function → the ordinal its continuation takes in its own chain.
    pub continuations: HashMap<AnonymousEnclosingFunction, u32>,
}

/// One chain of enclosing names, and the source function it lies in.
#[derive(Clone)]
struct Chain {
    segments: Vec<String>,
    enclosing_function: Option<AnonymousEnclosingFunction>,
}

impl Chain {
    fn named(&self, name: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(name.to_string());
        Self {
            segments,
            enclosing_function: self.enclosing_function,
        }
    }

    fn in_function(mut self, function: AnonymousEnclosingFunction) -> Self {
        self.enclosing_function = Some(function);
        self
    }

    fn spelling(&self) -> String {
        self.segments.join("$")
    }
}

enum Child {
    Expr(ExprId),
    Stmt(StmtId),
}

struct Inventor<'a> {
    file: &'a File,
    counters: &'a mut HashMap<String, u32>,
    names: InventedLocalNames,
}

/// Invent the names of every local node `file` declares. `facade` is the simple name of the file
/// facade; `counters` carries the per-chain sequences across the declaration units of one file.
pub(super) fn invent(
    file: &File,
    facade: &str,
    counters: &mut HashMap<String, u32>,
) -> InventedLocalNames {
    let mut inventor = Inventor {
        file,
        counters,
        names: InventedLocalNames::default(),
    };
    let facade_chain = Chain {
        segments: vec![facade.to_string()],
        enclosing_function: None,
    };
    let anonymous = file
        .anonymous_object_classes
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let anonymous_names = anonymous
        .iter()
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Class(class) => Some(format!("{}.", class.name)),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .collect::<Vec<_>>();
    for &declaration in &file.decls {
        if anonymous.contains(&declaration) || file.is_local_declaration(declaration) {
            continue;
        }
        match file.decl(declaration) {
            Decl::Fun(function) => inventor.function(
                function,
                &facade_chain,
                Some(AnonymousEnclosingFunction::TopLevel(declaration)),
            ),
            Decl::Property(property) => inventor.property(property, &facade_chain),
            Decl::Class(class) => {
                // A classifier nested in an anonymous object is walked with the object that
                // declares it, where its chain is known.
                if anonymous_names
                    .iter()
                    .any(|prefix| class.name.starts_with(prefix.as_str()))
                {
                    continue;
                }
                let chain = Chain {
                    segments: vec![class.name.replace('.', "$")],
                    enclosing_function: None,
                };
                inventor.class_body(declaration, class, &chain, false);
            }
        }
    }
    inventor.names
}

impl Inventor<'_> {
    /// The next position of `chain`'s sequence, as a chain one segment longer.
    fn next(&mut self, chain: &Chain) -> Chain {
        let counter = self
            .counters
            .entry(chain.spelling().to_ascii_uppercase())
            .or_insert(0);
        *counter += 1;
        let ordinal = *counter;
        chain.named(&ordinal.to_string())
    }

    fn function(
        &mut self,
        function: &FunDecl,
        outer: &Chain,
        identity: Option<AnonymousEnclosingFunction>,
    ) {
        let mut chain = outer.named(&function.name);
        if let Some(identity) = identity {
            chain = chain.in_function(identity);
        }
        if function.is_suspend() && !matches!(function.body, FunBody::None) {
            let continuation = self.next(&outer.named(&function.name));
            if let Some(identity) = identity {
                let ordinal = continuation
                    .segments
                    .last()
                    .and_then(|ordinal| ordinal.parse().ok())
                    .expect("a continuation position is an ordinal");
                self.names.continuations.insert(identity, ordinal);
            }
        }
        for default in function
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
        {
            self.expr(default, &chain);
        }
        self.body(&function.body, &chain);
    }

    fn body(&mut self, body: &FunBody, chain: &Chain) {
        match body {
            FunBody::Expr(root) | FunBody::Block(root) => self.expr(*root, chain),
            FunBody::None => {}
        }
    }

    fn property(&mut self, property: &PropDecl, outer: &Chain) {
        let chain = outer.named(&property.name);
        let delegated = property.delegate.is_some();
        if delegated {
            self.next(&chain);
        }
        for initializer in property.init.iter().chain(property.delegate.iter()) {
            self.expr(*initializer, &chain);
        }
        if delegated {
            // The accessors pass a reference to the property itself to `getValue`/`setValue`.
            self.next(&chain);
            if property.is_var {
                self.next(&chain);
            }
        }
        if let Some(getter) = &property.getter {
            self.body(getter, &chain);
        }
        if let Some(body) = property
            .setter
            .as_ref()
            .and_then(|setter| setter.body.as_ref())
        {
            self.body(body, &chain);
        }
    }

    /// Walk a classifier's members in `chain`. `anonymous` is set for an anonymous object, whose
    /// super-constructor arguments its construction site has already walked in the outer chain.
    fn class_body(
        &mut self,
        declaration: DeclId,
        class: &ClassDecl,
        chain: &Chain,
        anonymous: bool,
    ) {
        // The constructor contributes no name: its parameters' defaults, the superclass arguments
        // and every `init` block are numbered in the class's own chain.
        for default in class.props.iter().filter_map(|parameter| parameter.default) {
            self.expr(default, chain);
        }
        if !anonymous {
            for argument in &class.base_args {
                self.expr(*argument, chain);
            }
        }
        // The `$$delegate_N` field adds no name of its own, like every field.
        for delegation in &class.interface_delegations {
            self.expr(delegation.value, chain);
        }
        for entry in &class.enum_entries {
            // An entry with a body is an instance of its own class, whose constructor passes the
            // arguments on: they are numbered in that class's chain, not the enum's.
            let has_body = !entry.methods.is_empty()
                || !entry.props.is_empty()
                || !entry.init_order.is_empty();
            let entry_chain = if has_body {
                chain.named(&entry.name)
            } else {
                chain.clone()
            };
            for argument in &entry.args {
                self.expr(*argument, &entry_chain);
            }
            if has_body {
                self.initializers(&entry.init_order, &entry.props, &entry_chain);
                for method in &entry.methods {
                    self.function(method, &entry_chain, None);
                }
            }
        }
        // `init` blocks and secondary constructors share the class's chain, in source order.
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
                        self.secondary_constructor(constructor, chain);
                    }
                    self.expr(*block, chain);
                }
                ClassInit::PropInit(index) => self.property(&class.body_props[*index], chain),
            }
        }
        for constructor in constructors {
            self.secondary_constructor(constructor, chain);
        }
        for (index, method) in class.methods.iter().enumerate() {
            let identity = AnonymousEnclosingFunction::Member {
                class: declaration,
                method: u32::try_from(index).expect("too many class methods"),
            };
            self.function(method, chain, Some(identity));
        }
    }

    fn initializers(&mut self, order: &[ClassInit], properties: &[PropDecl], chain: &Chain) {
        for step in order {
            match step {
                ClassInit::Block(block) => self.expr(*block, chain),
                ClassInit::PropInit(index) => self.property(&properties[*index], chain),
            }
        }
    }

    fn secondary_constructor(&mut self, constructor: &crate::ast::SecondaryCtor, chain: &Chain) {
        for default in constructor
            .params
            .iter()
            .filter_map(|parameter| parameter.default)
        {
            self.expr(default, chain);
        }
        match &constructor.delegation {
            CtorDelegation::This(call) | CtorDelegation::Super(call) => {
                for argument in &call.args {
                    self.expr(*argument, chain);
                }
            }
            CtorDelegation::None => {}
        }
        if let Some(body) = constructor.body {
            self.expr(body, chain);
        }
    }

    fn expr(&mut self, expression: ExprId, chain: &Chain) {
        let file = self.file;
        match file.expr(expression) {
            Expr::Lambda { body, .. } => {
                let own = self.next(chain);
                self.expr(*body, &own);
            }
            // A bound receiver is walked inside the reference, like the lambda body it becomes.
            Expr::CallableRef { receiver, name } if name != "class" => {
                let own = self.next(chain);
                if let Some(receiver) = receiver {
                    self.expr(*receiver, &own);
                }
            }
            Expr::Call { .. } if file.anonymous_object_classes.contains_key(&expression) => {
                let declaration = file.anonymous_object_classes[&expression];
                let own = self.next(chain);
                let Decl::Class(class) = file.decl(declaration) else {
                    return;
                };
                self.names.anonymous_objects.push((
                    expression,
                    declaration,
                    own.spelling(),
                    chain.enclosing_function,
                ));
                for argument in &class.base_args {
                    self.expr(*argument, chain);
                }
                let own = Chain {
                    segments: own.segments,
                    enclosing_function: chain.enclosing_function,
                };
                self.class_body(declaration, class, &own, true);
                self.nested_classifiers(&class.name, &own);
            }
            _ => {
                // Children in evaluation order; a block interleaves statements with none.
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
                        Child::Expr(child) => self.expr(child, chain),
                        Child::Stmt(statement) => self.stmt(statement, chain),
                    }
                }
            }
        }
    }

    /// Walk the classifiers the parser hoisted out of a local class or anonymous object, which it
    /// named by the path from that class (`<class>.Inner`).
    fn nested_classifiers(&mut self, owner: &str, chain: &Chain) {
        let file = self.file;
        let prefix = format!("{owner}.");
        for &declaration in &file.decls {
            let Decl::Class(class) = file.decl(declaration) else {
                continue;
            };
            let Some(rest) = class.name.strip_prefix(prefix.as_str()) else {
                continue;
            };
            if rest.contains('.') {
                continue;
            }
            let nested = chain.named(rest);
            self.class_body(declaration, class, &nested, false);
            self.nested_classifiers(&class.name, &nested);
        }
    }

    fn stmt(&mut self, statement: StmtId, chain: &Chain) {
        let file = self.file;
        match file.stmt(statement) {
            Stmt::Local { name, init, .. } => self.expr(*init, &chain.named(name)),
            Stmt::LocalDelegate {
                name,
                is_var,
                delegate,
                ..
            } => {
                let property = chain.named(name);
                self.next(&property);
                self.expr(*delegate, &property);
                // Its accessors are local functions without a source name.
                self.next(&property);
                if *is_var {
                    self.next(&property);
                }
            }
            Stmt::LocalFun(function) => self.function(function, chain, None),
            Stmt::LocalClass(_) => {
                // The local class keeps the runtime name the parser hoisted it under; what it
                // declares is numbered in that name's chain.
                if let Some(&declaration) = file.local_class_decls.get(&statement) {
                    if let Decl::Class(hoisted) = file.decl(declaration) {
                        let own = Chain {
                            segments: vec![hoisted.name.replace('.', "$")],
                            enclosing_function: chain.enclosing_function,
                        };
                        self.class_body(declaration, hoisted, &own, false);
                        self.nested_classifiers(&hoisted.name, &own);
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
                    self.expr(child, chain);
                }
            }
        }
    }
}
