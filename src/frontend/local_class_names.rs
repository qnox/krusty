//! Backend-neutral naming provenance for classes a file declares locally.
//!
//! kotlinc names every local class, anonymous object, lambda, callable reference and suspend
//! continuation in ONE walk over the file (`InventNamesForLocalClasses`), before any of them is
//! lowered: a lambda compiled through `invokedynamic` still takes its position in the sequence even
//! though no class is ever written for it. This module is that walk, over the source tree.
//!
//! This pass records exact source ownership, named lexical segments, and generated-artifact
//! ordinals. It deliberately does not know a file facade or a target separator. A backend combines
//! this contract with its physical owner and naming rules.
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
    File, FunBody, FunDecl, LocalClassNameProvenance, PropDecl, Stmt, StmtId,
};

/// The names one walk invents for a file's local nodes.
#[derive(Default)]
pub(super) struct InventedLocalNames {
    /// Local/anonymous declaration → its target-neutral lexical provenance.
    pub classes: HashMap<DeclId, LocalClassNameProvenance>,
    /// Anonymous-object class → the source function that lexically encloses it.
    pub anonymous_enclosing_functions: HashMap<DeclId, AnonymousEnclosingFunction>,
    /// Suspend function → the ordinal its continuation takes in its own chain.
    pub continuations: HashMap<AnonymousEnclosingFunction, u32>,
}

/// One chain of enclosing names, and the source function it lies in.
#[derive(Clone)]
struct Chain {
    /// Exact source classifier at the target-independent ownership boundary. `None` means file.
    owner: Option<DeclId>,
    /// Stable source identity used only to continue a sequence across bounded reparses.
    counter_owner: String,
    segments: Vec<String>,
    enclosing_function: Option<AnonymousEnclosingFunction>,
}

impl Chain {
    fn named(&self, name: &str) -> Self {
        let mut segments = self.segments.clone();
        segments.push(name.to_string());
        Self {
            owner: self.owner,
            counter_owner: self.counter_owner.clone(),
            segments,
            enclosing_function: self.enclosing_function,
        }
    }

    fn in_function(mut self, function: AnonymousEnclosingFunction) -> Self {
        self.enclosing_function = Some(function);
        self
    }

    fn counter_key(&self) -> Vec<String> {
        let mut key = Vec::with_capacity(self.segments.len() + 1);
        key.push(self.counter_owner.to_ascii_uppercase());
        key.extend(
            self.segments
                .iter()
                .map(|segment| segment.to_ascii_uppercase()),
        );
        key
    }

    fn provenance(&self, ordinal: Option<u32>) -> LocalClassNameProvenance {
        LocalClassNameProvenance {
            lexical_owner: self.owner,
            segments: self.segments.clone(),
            ordinal,
        }
    }
}

enum Child {
    Expr(ExprId),
    Stmt(StmtId),
}

struct Inventor<'a> {
    file: &'a File,
    counters: &'a mut HashMap<Vec<String>, u32>,
    names: InventedLocalNames,
}

/// Record the provenance of every local node `file` declares. `counters` carries the per-chain
/// sequences across the declaration units of one file.
pub(super) fn invent(file: &File, counters: &mut HashMap<Vec<String>, u32>) -> InventedLocalNames {
    let mut inventor = Inventor {
        file,
        counters,
        names: InventedLocalNames::default(),
    };
    let file_chain = Chain {
        owner: None,
        counter_owner: "file".to_string(),
        segments: Vec::new(),
        enclosing_function: None,
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
            Decl::Fun(function) => inventor.function(
                function,
                &file_chain,
                Some(AnonymousEnclosingFunction::TopLevel(declaration)),
            ),
            Decl::Property(property) => inventor.property(property, &file_chain),
            Decl::Class(class) => {
                let chain = Chain {
                    owner: Some(declaration),
                    counter_owner: format!("class:{}", class.name),
                    segments: Vec::new(),
                    enclosing_function: None,
                };
                inventor.class_body(declaration, class, &chain, false);
            }
        }
    }
    if let Some(script) = file.script_body {
        inventor.expr(script, &file_chain);
    }
    inventor.names
}

impl Inventor<'_> {
    /// The next position of `chain`'s sequence, as a chain one segment longer.
    fn next(&mut self, chain: &Chain) -> Chain {
        let counter = self.counters.entry(chain.counter_key()).or_insert(0);
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
            // A class literal has no generated callable-reference class and therefore consumes no
            // position. Its bound expression, when present, remains in the surrounding chain.
            Expr::CallableRef { receiver, .. }
                if file.class_literal_references.contains(&expression.0) =>
            {
                if let Some(receiver) = receiver {
                    self.expr(*receiver, chain);
                }
            }
            // A bound receiver is walked inside the reference, like the lambda body it becomes.
            Expr::CallableRef { receiver, .. } => {
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
                let ordinal = own
                    .segments
                    .last()
                    .and_then(|ordinal| ordinal.parse().ok())
                    .expect("an anonymous-object position is an ordinal");
                self.names
                    .classes
                    .insert(declaration, chain.provenance(Some(ordinal)));
                if let Some(function) = chain.enclosing_function {
                    self.names
                        .anonymous_enclosing_functions
                        .insert(declaration, function);
                }
                for argument in &class.base_args {
                    self.expr(*argument, chain);
                }
                let own = Chain {
                    owner: Some(declaration),
                    counter_owner: format!("anonymous:{}", declaration.0),
                    segments: Vec::new(),
                    enclosing_function: chain.enclosing_function,
                };
                self.class_body(declaration, class, &own, true);
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
            Stmt::LocalClass(source) => {
                if let Some(&declaration) = file.local_class_decls.get(&statement) {
                    if let Decl::Class(hoisted) = file.decl(declaration) {
                        self.names
                            .classes
                            .insert(declaration, chain.named(&source.name).provenance(None));
                        let own = Chain {
                            owner: Some(declaration),
                            counter_owner: format!("local:{}", declaration.0),
                            segments: Vec::new(),
                            enclosing_function: chain.enclosing_function,
                        };
                        self.class_body(declaration, hoisted, &own, false);
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
