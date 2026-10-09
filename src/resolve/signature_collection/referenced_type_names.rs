//! Written type-name inventory for source signature collection.
//!
//! This walk collects only lookup input from compact headers or the retained inspection AST.
//! Binding still belongs to the source type universe and the ordinary scope/import machinery.

use super::*;

/// Collect every simple type NAME referenced in a `TypeRef` (recursively through generic arguments,
/// function-type params/return, and the array/return `arg`) into `out`.
pub(super) fn collect_typeref_names(r: &TypeRef, out: &mut std::collections::HashSet<String>) {
    if !r.name.is_empty() && r.name != "<fun>" {
        out.insert(r.name.clone());
    }
    if let Some(a) = &r.arg {
        collect_typeref_names(a, out);
    }
    for t in &r.targs {
        collect_typeref_names(t, out);
    }
    for p in &r.fun_params {
        collect_typeref_names(p, out);
    }
}

fn collect_declaration_type_parameter_annotation_names(
    file: &File,
    declaration_start: u32,
    out: &mut std::collections::HashSet<String>,
) {
    if let Some(parameters) = file
        .declaration_type_parameter_annotations
        .get(&declaration_start)
    {
        out.extend(
            parameters
                .iter()
                .flat_map(|parameter| &parameter.annotations)
                .map(|annotation| annotation.name.clone()),
        );
    }
}

fn collect_fun_type_names(file: &File, f: &FunDecl, out: &mut std::collections::HashSet<String>) {
    collect_declaration_type_parameter_annotation_names(file, f.signature_span.lo, out);
    if let Some(receiver) = &f.receiver {
        collect_typeref_names(receiver, out);
    }
    for parameter in &f.params {
        collect_typeref_names(&parameter.ty, out);
    }
    if let Some(ret) = &f.ret {
        collect_typeref_names(ret, out);
    }
    for (_, bound) in &f.type_param_bounds {
        collect_typeref_names(bound, out);
    }
}

fn collect_property_type_names(
    file: &File,
    p: &PropDecl,
    out: &mut std::collections::HashSet<String>,
) {
    collect_declaration_type_parameter_annotation_names(file, p.span.lo, out);
    for parameter in &p.context_params {
        collect_typeref_names(&parameter.ty, out);
    }
    if let Some(receiver) = &p.receiver {
        collect_typeref_names(receiver, out);
    }
    if let Some(ty) = &p.ty {
        collect_typeref_names(ty, out);
    }
    if let Some(ty) = p
        .explicit_backing_field
        .as_ref()
        .and_then(|field| field.ty.as_ref())
    {
        collect_typeref_names(ty, out);
    }
}

fn collect_expression_type_names(
    file: &File,
    roots: impl IntoIterator<Item = ExprId>,
    out: &mut std::collections::HashSet<String>,
) {
    let mut expressions = roots.into_iter().collect::<Vec<_>>();
    let mut statements = Vec::new();
    let mut seen_expressions = std::collections::HashSet::new();
    let mut seen_statements = std::collections::HashSet::new();
    while let Some(expression) = expressions.pop() {
        if !seen_expressions.insert(expression) {
            continue;
        }
        match file.expr(expression) {
            Expr::Name(name) => {
                out.insert(name.clone());
            }
            Expr::Is { ty, .. } | Expr::As { ty, .. } => collect_typeref_names(ty, out),
            Expr::Try { catches, .. } => {
                for catch in catches {
                    collect_typeref_names(&catch.ty, out);
                }
            }
            _ => {}
        }
        if let Some(arguments) = file.call_type_args.get(&expression.0) {
            for argument in arguments {
                collect_typeref_names(argument, out);
            }
        }
        if let Some(parameters) = file.lambda_param_types.get(&expression.0) {
            for parameter in parameters.iter().flatten() {
                collect_typeref_names(parameter, out);
            }
        }
        if let Some(ret) = file.anon_fun_ret.get(&expression.0) {
            collect_typeref_names(ret, out);
        }
        if let Some(receiver) = file.anon_fun_receivers.get(&expression.0) {
            collect_typeref_names(receiver, out);
        }
        file.any_child_expr(
            expression,
            &mut |child| {
                expressions.push(child);
                false
            },
            &mut |statement| {
                statements.push(statement);
                false
            },
        );
        while let Some(statement) = statements.pop() {
            if !seen_statements.insert(statement) {
                continue;
            }
            match file.stmt(statement) {
                Stmt::LocalLateinit { ty, .. }
                | Stmt::Local { ty: Some(ty), .. }
                | Stmt::LocalDelegate { ty: Some(ty), .. } => collect_typeref_names(ty, out),
                Stmt::LocalFun(function) => collect_fun_type_names(file, function, out),
                Stmt::LocalClass(class) => collect_class_type_names(file, class, out),
                Stmt::LocalTypeAlias(alias) => collect_typeref_names(&alias.target, out),
                _ => {}
            }
            file.any_child_stmt(statement, &mut |child| {
                expressions.push(child);
                false
            });
        }
    }
}

pub(super) fn fun_expression_roots(function: &FunDecl) -> impl Iterator<Item = ExprId> + '_ {
    function
        .params
        .iter()
        .filter_map(|parameter| parameter.default)
        .chain(
            function
                .params
                .iter()
                .flat_map(|parameter| parameter.annotation_args.iter().flatten().copied()),
        )
        .chain(match function.body {
            FunBody::Expr(body) | FunBody::Block(body) => Some(body),
            FunBody::None => None,
        })
}

pub(super) fn property_expression_roots(property: &PropDecl) -> impl Iterator<Item = ExprId> + '_ {
    property
        .context_params
        .iter()
        .filter_map(|parameter| parameter.default)
        .chain(
            property
                .context_params
                .iter()
                .flat_map(|parameter| parameter.annotation_args.iter().flatten().copied()),
        )
        .chain(property.init)
        .chain(property.delegate)
        .chain(property.getter.as_ref().and_then(|body| match body {
            FunBody::Expr(body) | FunBody::Block(body) => Some(*body),
            FunBody::None => None,
        }))
        .chain(
            property
                .setter
                .as_ref()
                .and_then(|setter| setter.body.as_ref())
                .and_then(|body| match body {
                    FunBody::Expr(body) | FunBody::Block(body) => Some(*body),
                    FunBody::None => None,
                }),
        )
}

pub(super) fn collect_class_type_names(
    file: &File,
    class: &ClassDecl,
    out: &mut std::collections::HashSet<String>,
) {
    collect_declaration_type_parameter_annotation_names(file, class.span.lo, out);
    for supertype in &class.supertypes {
        collect_typeref_names(supertype, out);
    }
    out.extend(class.base_class.iter().cloned());
    out.extend(
        class
            .interface_delegations
            .iter()
            .map(|delegation| delegation.interface.clone()),
    );
    for (_, bound) in &class.type_param_bounds {
        collect_typeref_names(bound, out);
    }
    for parameter in &class.props {
        collect_typeref_names(&parameter.ty, out);
    }
    for property in &class.body_props {
        collect_property_type_names(file, property, out);
    }
    for method in &class.methods {
        collect_fun_type_names(file, method, out);
    }
    for entry in &class.enum_entries {
        for method in &entry.methods {
            collect_fun_type_names(file, method, out);
        }
        for property in &entry.props {
            collect_property_type_names(file, property, out);
        }
    }
    for constructor in &class.secondary_ctors {
        for parameter in &constructor.params {
            collect_typeref_names(&parameter.ty, out);
        }
    }
    let expression_roots = class
        .annotation_args
        .iter()
        .flatten()
        .copied()
        .chain(class.props.iter().filter_map(|parameter| parameter.default))
        .chain(
            class
                .props
                .iter()
                .flat_map(|parameter| parameter.annotation_args.iter().flatten().copied()),
        )
        .chain(class.base_args.iter().copied())
        .chain(
            class
                .interface_delegations
                .iter()
                .map(|delegation| delegation.value),
        )
        .chain(class.body_props.iter().flat_map(property_expression_roots))
        .chain(class.methods.iter().flat_map(fun_expression_roots))
        .chain(class.init_order.iter().filter_map(|init| match init {
            crate::ast::ClassInit::Block(body) => Some(*body),
            crate::ast::ClassInit::PropInit(_) => None,
        }))
        .chain(class.secondary_ctors.iter().flat_map(|constructor| {
            constructor
                .params
                .iter()
                .filter_map(|parameter| parameter.default)
                .chain(
                    constructor
                        .params
                        .iter()
                        .flat_map(|parameter| parameter.annotation_args.iter().flatten().copied()),
                )
                .chain(match &constructor.delegation {
                    crate::ast::CtorDelegation::This(arguments)
                    | crate::ast::CtorDelegation::Super(arguments) => arguments.args.clone(),
                    crate::ast::CtorDelegation::None => Vec::new(),
                })
                .chain(constructor.body)
        }))
        .chain(class.enum_entries.iter().flat_map(|entry| {
            entry
                .annotation_args
                .iter()
                .flatten()
                .copied()
                .chain(entry.args.iter().copied())
                .chain(entry.methods.iter().flat_map(fun_expression_roots))
                .chain(entry.props.iter().flat_map(property_expression_roots))
                .chain(entry.init_order.iter().filter_map(|step| match step {
                    crate::ast::ClassInit::Block(body) => Some(*body),
                    crate::ast::ClassInit::PropInit(_) => None,
                }))
        }));
    collect_expression_type_names(file, expression_roots, out);
}

/// Collect names that signature inference may need to resolve through imports.
pub(super) fn collect_streamed_header_type_names(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
    out: &mut std::collections::HashSet<String>,
) {
    for root in headers.detached_type_roots(source).chain(
        headers
            .stubs
            .iter()
            .filter(|stub| stub.source == source)
            .flat_map(|stub| headers.syntax.declaration_type_roots(stub.id)),
    ) {
        if let Some(reference) = headers
            .syntax
            .transient_type_ref(root, &headers.lookup_names)
        {
            collect_typeref_names(&reference, out);
        }
    }
}

/// Collect names that signature inference may need to resolve through imports for the legacy
/// inspection pipeline. Production uses [`collect_streamed_header_type_names`] and cannot observe
/// ordinary expression or statement arenas here.
pub(super) fn collect_file_type_names(file: &File, out: &mut std::collections::HashSet<String>) {
    for reference in &file.detached_type_refs {
        collect_typeref_names(reference, out);
    }
    // Every bare VALUE reference (`val x = EmptyCoroutineContext` — an object singleton/top-level fun
    // used as a value) is a candidate too: a wildcard/explicit import resolves it no differently from a
    // type. Collecting all `Expr::Name`s over-approximates (locals, params), but a name that matches no
    // import package simply isn't added — harmless. This is what lets a default-import-only seed (no
    // whole-classpath blanket) still resolve imported values.
    for e in &file.expr_arena {
        match e {
            Expr::Name(n) => {
                out.insert(n.clone());
            }
            // Type names in EXPRESSION position — a cast (`x as T`), a type test (`x is T`), or a catch
            // clause (`catch (e: T)`) — are just as much candidates as a declared parameter type, and are
            // the only place some names (a caught `NotImplementedError`) appear. The signature phase's
            // `ty_of_ref` / `catch_internal` resolve them through the same `class_names`, so they must be
            // import-resolved here too.
            Expr::Is { ty, .. } | Expr::As { ty, .. } => collect_typeref_names(ty, out),
            Expr::Try { catches, .. } => {
                for c in catches {
                    collect_typeref_names(&c.ty, out);
                }
            }
            _ => {}
        }
    }
    // A local declaration's type annotation (`val r: Reg`, `lateinit var x: T`) is a type-position name
    // too, and may be the only place a name appears (a classpath alias used only for a local `val`).
    for s in &file.stmt_arena {
        match s {
            Stmt::LocalLateinit { ty, .. } => collect_typeref_names(ty, out),
            Stmt::Local { ty: Some(ty), .. } | Stmt::LocalDelegate { ty: Some(ty), .. } => {
                collect_typeref_names(ty, out)
            }
            Stmt::LocalFun(f) => collect_fun_type_names(file, f, out),
            Stmt::LocalClass(class) => collect_class_type_names(file, class, out),
            Stmt::LocalTypeAlias(alias) => collect_typeref_names(&alias.target, out),
            _ => {}
        }
    }
    // Explicit call type arguments (`foo<Bar>()`, `arrayOf<Baz>()`) are type-position names too.
    for targs in file.call_type_args.values() {
        for t in targs {
            collect_typeref_names(t, out);
        }
    }
    // A `typealias A = Foo` TARGET is a candidate: alias expansion resolves `A` by looking up `Foo` in
    // the resolved names, so `Foo` must itself be import-resolved (it may not appear in any other type
    // position). Both simple (`Foo`) and dotted (`a.b.Foo`) targets go in; the resolver tries each.
    for (_, target) in &file.type_aliases {
        out.insert(target.clone());
    }
    for &declaration in &file.decls {
        match file.decl(declaration) {
            Decl::Fun(function) => collect_fun_type_names(file, function, out),
            Decl::Property(property) => collect_property_type_names(file, property, out),
            Decl::Class(class) => collect_class_type_names(file, class, out),
        }
    }
}
