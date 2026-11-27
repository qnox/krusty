//! Callable identities in selected bodies and their reached closure implementations.

use super::*;
use crate::fx_hash::HashSet;

/// The forms common lowering realizes differently from the calls they serialize: the closures
/// whose implementations it lowers with the body, and the calls it realizes as a common-IR
/// operation instead of an edge to the declaration they name. The policy is common lowering's;
/// the arena only walks.
#[derive(Clone, Copy)]
pub(crate) struct EmittedCallForms {
    pub(crate) closure: fn(&KlibIrArena, &KlibIrExpr) -> Option<KlibIrFunctionId>,
    pub(crate) folded_call: fn(&KlibIrArena, &KlibIrExpr) -> bool,
    /// The calls inside an expression that the form the expression is realizes without their
    /// declarations, such as an array `for` loop's iterator protocol.
    pub(crate) folded_calls: fn(&KlibIrArena, &KlibIrExpr) -> Vec<KlibIrExprId>,
    /// The declaration a call is realized as a call of in place of the one it names, such as the
    /// array content extension a data class array member operator lowers to.
    pub(crate) implied_callee: fn(&KlibIrArena, &KlibIrExpr) -> Option<&'static KlibIrSymbol>,
    /// The functional interface a closure is converted to, when its serialized form is a closure
    /// of the interface's type rather than a `SAM_CONVERSION` of a function value.
    pub(crate) converted_closure:
        for<'a> fn(&'a KlibIrArena, &KlibIrExpr) -> Option<&'a KlibIrSymbol>,
}

/// What the bodies reached from a set of roots emit: their calls, and the objects and enum
/// entries whose values they read.
struct EmittedWalk<'a> {
    calls: Vec<&'a KlibIrSymbol>,
    objects: Vec<&'a KlibIrSymbol>,
    sams: Vec<&'a KlibIrSymbol>,
    enum_entries: Vec<&'a KlibIrSymbol>,
    /// The declarations called with a `super` qualifier, which run without dispatch.
    supers: Vec<&'a KlibIrSymbol>,
}

impl KlibIrArena {
    /// Closure selection belongs to common lowering; this arena supplies only the structural walk.
    /// The function's own parameter defaults are roots, since its default-argument entry runs
    /// them; unreferenced local declarations never are. The direct-only inventories remain
    /// separate, particularly the one that authenticates intrinsic support.
    pub(crate) fn emitted_callable_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        self.emitted_walk(
            self.function_expression_roots(function),
            forms,
            HashSet::from_iter([function.base.symbol.clone()]),
        )
        .calls
    }

    fn function_expression_roots(&self, function: &KlibIrFunction) -> Vec<KlibIrExprId> {
        let mut roots = Vec::new();
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut roots);
        }
        roots.extend(
            function
                .context_parameters
                .iter()
                .chain(&function.extension_receiver)
                .chain(&function.regular_parameters)
                .filter_map(|parameter| parameter.default_value),
        );
        roots
    }

    pub(crate) fn statement_expression_roots(
        &self,
        statements: &[KlibIrStatement],
    ) -> Vec<KlibIrExprId> {
        let mut roots = Vec::new();
        push_statement_expressions(self, statements, &mut roots);
        roots
    }

    /// Class initialization owns these roots; it uses the same reached-closure policy as bodies.
    pub(crate) fn emitted_callable_symbols_from(
        &self,
        roots: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_calls_from(roots, forms, HashSet::default())
    }

    fn emitted_calls_from(
        &self,
        pending: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
        functions: HashSet<KlibIrSymbol>,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_walk(pending, forms, functions).calls
    }

    /// The objects whose values the bodies reached from `roots` read, through the closures
    /// `forms` lowers with them. A support unit creates each such object's instance.
    pub(crate) fn emitted_object_symbols_from(
        &self,
        roots: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_walk(roots, forms, HashSet::default()).objects
    }

    /// The enum entries whose values the bodies reached from `roots` read, through the closures
    /// `forms` lowers with them. A support unit builds each such entry's enum class.
    pub(crate) fn emitted_enum_entry_symbols_from(
        &self,
        roots: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_walk(roots, forms, HashSet::default())
            .enum_entries
    }

    /// [`Self::emitted_object_symbols_from`] of `function`'s body and parameter defaults.
    pub(crate) fn emitted_object_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        self.emitted_function_walk(function, forms).objects
    }

    /// [`Self::emitted_enum_entry_symbols_from`] of `function`'s body and parameter defaults.
    pub(crate) fn emitted_enum_entry_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        self.emitted_function_walk(function, forms).enum_entries
    }

    /// The declarations `function`'s body calls with a `super` qualifier: each runs without
    /// dispatch, so its body is emitted wherever the receiver's class does not realize it.
    pub(crate) fn emitted_super_call_symbols<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        self.emitted_function_walk(function, forms).supers
    }

    /// [`Self::emitted_super_call_symbols`] of the bodies reached from `roots`, as a file
    /// initializer or a local class member is.
    pub(crate) fn emitted_super_call_symbols_from(
        &self,
        roots: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_walk(roots, forms, HashSet::default()).supers
    }

    fn emitted_function_walk<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> EmittedWalk<'a> {
        let mut roots = Vec::new();
        let mut functions = HashSet::default();
        functions.insert(function.base.symbol.clone());
        if let Some(KlibIrBody::Block(statements)) = &function.body {
            push_statement_expressions(self, statements, &mut roots);
        }
        roots.extend(
            function
                .context_parameters
                .iter()
                .chain(&function.extension_receiver)
                .chain(&function.regular_parameters)
                .filter_map(|parameter| parameter.default_value),
        );
        self.emitted_walk(roots, forms, functions)
    }

    pub(crate) fn emitted_sam_classifiers<'a>(
        &'a self,
        function: &'a KlibIrFunction,
        forms: EmittedCallForms,
    ) -> Vec<&'a KlibIrSymbol> {
        self.emitted_function_walk(function, forms).sams
    }

    pub(crate) fn emitted_sam_classifiers_from(
        &self,
        roots: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        self.emitted_walk(roots, forms, HashSet::default()).sams
    }

    /// The emitted calls and the object and enum entry values reached from `pending`, through
    /// reached closures.
    fn emitted_walk(
        &self,
        mut pending: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
        mut functions: HashSet<KlibIrSymbol>,
    ) -> EmittedWalk<'_> {
        let mut expressions = HashSet::default();
        let mut calls = self.unfolded_call_symbols(pending.clone(), forms);
        let mut objects = Vec::new();
        let mut sams = Vec::new();
        let mut enum_entries = Vec::new();
        let mut supers = Vec::new();
        while let Some(id) = pending.pop() {
            if !expressions.insert(id) {
                continue;
            }
            let expression = self.expr(id);
            if let KlibIrExprKind::Call {
                access,
                super_qualifier: Some(_),
            } = &expression.kind
            {
                supers.push(&access.symbol);
            }
            match &expression.kind {
                KlibIrExprKind::GetObject(symbol) => objects.push(symbol),
                KlibIrExprKind::GetEnumValue(symbol) => enum_entries.push(symbol),
                _ => {}
            }
            if let KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::SamConversion,
                operand,
                ..
            } = &expression.kind
            {
                if let KlibIrType::Simple { classifier, .. } = self.ty(*operand) {
                    sams.push(classifier);
                }
            }
            sams.extend((forms.converted_closure)(self, expression));
            push_child_expressions(self, &expression.kind, &mut pending);
            if let Some(nested) = (forms.closure)(self, expression) {
                let nested = self.function(nested);
                if functions.insert(nested.base.symbol.clone()) {
                    if let Some(KlibIrBody::Block(statements)) = &nested.body {
                        let mut roots = Vec::new();
                        push_statement_expressions(self, statements, &mut roots);
                        calls.extend(self.unfolded_call_symbols(roots.clone(), forms));
                        pending.extend(roots);
                    }
                }
            }
        }
        EmittedWalk {
            calls,
            objects,
            sams,
            enum_entries,
            supers,
        }
    }

    /// The callable symbols reached from `pending`, except those of calls `forms` fold.
    fn unfolded_call_symbols(
        &self,
        mut pending: Vec<KlibIrExprId>,
        forms: EmittedCallForms,
    ) -> Vec<&KlibIrSymbol> {
        let mut calls = Vec::new();
        // A form is visited before the expressions it holds, so its folded calls are known when
        // the walk reaches them.
        let mut folded_calls = HashSet::default();
        while let Some(id) = pending.pop() {
            let expression = self.expr(id);
            push_child_expressions(self, &expression.kind, &mut pending);
            folded_calls.extend((forms.folded_calls)(self, expression));
            calls.extend((forms.implied_callee)(self, expression));
            match &expression.kind {
                KlibIrExprKind::Call { access, .. }
                    if !folded_calls.contains(&id) && !(forms.folded_call)(self, expression) =>
                {
                    calls.push(&access.symbol)
                }
                KlibIrExprKind::ConstructorCall { access, .. }
                | KlibIrExprKind::DelegatingConstructorCall(access)
                | KlibIrExprKind::EnumConstructorCall(access) => calls.push(&access.symbol),
                _ => {}
            }
        }
        calls
    }
}
