//! The `$childSerializers` cache: deciding which properties need one, and building it.
//!
//! A `@Serializable` class whose property has an ALLOCATED element serializer — a collection, an
//! enum — caches those serializers in a `Lazy[]` static so each is built once rather than at every
//! use. Which properties that covers, the static that holds them, and the accessor a `$serializer`
//! reaches it through are one responsibility, and it lives here rather than in the plugin's
//! top-level module.
//!
//! The pass runs SECOND, after every class's `$serializer` has been generated. A slot's element
//! serializer may be a sibling class's `$serializer` singleton, and the classes are generated in an
//! order that is not the source one. No single ordering could fix that — two `@Serializable`
//! classes may hold collections of each other — so the cache cannot be built while the loop that
//! creates those classes is still running.
use super::{
    class_ty,
    element_serializer::{
        child_cache_element_plan, emit_cached_element_serializer, ElementSerializerPlan,
        UnderivableChildCacheElement,
    },
    field_serializer_of, kserializer_of, property_is_contextual, type_name, Callee, ClassId,
    ExprId, InlineKind, IrConst, IrExpr, IrFile, IrFunction, IrTypeOp, PluginContext, Ty, TypeName,
};

/// One slot of the `Lazy[]` cache: a `Lazy<KSerializer<Any>>`, nullable because a property whose
/// serializer is a singleton contributes no entry.
pub(super) fn lazy_cache_element_ty() -> Ty {
    Ty::nullable(Ty::obj_args(
        "kotlin/Lazy",
        &[kserializer_of(class_ty("kotlin/Any"))],
    ))
}

/// The `Lazy[]` cache's own type.
pub(super) fn lazy_cache_ty() -> Ty {
    Ty::obj_args("kotlin/Array", &[lazy_cache_element_ty()])
}

/// The statement a reader puts at the top of its body: load the serialized class's
/// `$childSerializers` into `local`, through the accessor the PLAN names.
///
/// `ClassStatic` keeps that accessor's semantic identity — its name and JVM descriptor are formed
/// at the JVM boundary from the declaration itself, so neither can drift from what the plan
/// published. The local is DECLARED rather than merely assigned: its type is what tells the backend
/// an element load off it is an `aaload`, and a bare `SetValue` carries no declaration to read.
pub(super) fn load_cache_statement(
    ir: &mut IrFile,
    plan: &ChildSerializerCachePlan,
    local: u32,
    serialized: TypeName,
) -> ExprId {
    let read = ir.add_expr(IrExpr::Call {
        callee: Callee::ClassStatic {
            owner: serialized,
            function: plan.accessor,
        },
        dispatch_receiver: None,
        args: vec![],
    });
    ir.add_expr(IrExpr::Variable {
        index: local,
        ty: lazy_cache_ty(),
        init: Some(read),
        named: false,
    })
}

/// Element `k`'s serializer taken OUT of the cache for `childSerializers()`, or `None` when it has
/// no slot in this plan.
///
/// `childSerializers()` exposes a property's serializer shape, so a nullable property wraps the
/// cached base serializer here. Unlike a call-argument consumer, the result array's store does not
/// add a redundant `KSerializer` cast. The arity is checked against the caller's own element count
/// by [`ChildSerializerCachePlan::caches`].
pub(super) fn cached_element(
    ir: &mut IrFile,
    plan: Option<&ChildSerializerCachePlan>,
    cache_local: Option<u32>,
    k: usize,
    elements: usize,
    ty: &Ty,
) -> Option<ExprId> {
    let local = cache_local?;
    if !plan?.caches(k, elements) {
        return None;
    }
    let cached = read_cached_slot(ir, local, k);
    Some(if super::is_nullable(ty) {
        super::wrap_nullable_serializer(ir, cached)
    } else {
        cached
    })
}

/// Element `k`'s cached serializer narrowed from `Lazy.getValue()`'s erased `Object` to the
/// strategy interface consumed by an encode/decode call.
pub(super) fn cached_strategy(
    ir: &mut IrFile,
    plan: Option<&ChildSerializerCachePlan>,
    cache_local: Option<u32>,
    k: usize,
    elements: usize,
    strategy_interface: &str,
) -> Option<ExprId> {
    let local = cache_local?;
    if !plan?.caches(k, elements) {
        return None;
    }
    let cached = read_cached_slot(ir, local, k);
    Some(ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: cached,
        type_operand: class_ty(strategy_interface),
    }))
}

/// Read element `k` out of a `Lazy[]` cache already loaded into `cache_local` — `cache[k].value`.
///
/// All three readers (`childSerializers`, `deserialize`, `write$Self`) want exactly this, and the
/// shape is a property of how this module STORES the cache, so it is derived here once instead of
/// each reader repeating `kotlin/Lazy`'s member spelling. The value arrives as `Object`, which the
/// use site narrows to what it takes.
fn read_cached_slot(ir: &mut IrFile, cache_local: u32, k: usize) -> ExprId {
    let cache = ir.add_expr(IrExpr::GetValue(cache_local));
    let index = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
    let slot = ir.add_expr(IrExpr::Call {
        callee: Callee::Intrinsic {
            operation: crate::ir::IrIntrinsic::ArrayGet,
            ret: lazy_cache_element_ty(),
        },
        dispatch_receiver: Some(cache),
        args: vec![index],
    });
    ir.add_expr(IrExpr::Call {
        callee: super::virtual_iface("kotlin/Lazy", "getValue", "()Ljava/lang/Object;"),
        dispatch_receiver: Some(slot),
        args: vec![],
    })
}

/// What one class's `$childSerializers` cache IS, published by the pass that builds it.
///
/// Every reader — the `$serializer`'s `childSerializers` and `deserialize`, and the serialized
/// class's `write$Self` — takes its facts from here. None of them searches `ir.statics` for the
/// field's spelling, re-derives the owner from text, or re-answers which properties are cached:
/// a slot a reader believes in that the builder did not write is a null it dereferences, and the
/// only way to keep the two from drifting is for there to be one answer.
#[derive(Clone)]
pub(super) struct ChildSerializerCachePlan {
    /// The `$childSerializers` static's index in `IrFile::statics` — what `write$Self` reads
    /// directly, being a static member of the class that owns it.
    pub(super) static_index: u32,
    /// The `access$get$childSerializers$cp()` accessor a `$serializer` reaches the cache through,
    /// as a function id rather than a name to look up.
    pub(super) accessor: u32,
    /// Which serialized properties, in element order, the cache holds a slot for.
    pub(super) cached: Vec<bool>,
}

impl ChildSerializerCachePlan {
    /// Whether element `k` is cached, checked against the caller's own element count.
    ///
    /// The plan is published with exactly one entry per serialized element, so a length that
    /// disagrees is an invalid intermediate state rather than an "uncached" answer. Reading it as
    /// one would re-enter the legacy inline derivation for elements the serialized class DID give
    /// a slot, leaving a `$childSerializers` array that nothing reads and two derivations of the
    /// same element serializer in one class.
    pub(super) fn caches(&self, k: usize, elements: usize) -> bool {
        assert_eq!(
            self.cached.len(),
            elements,
            "the child-serializer cache plan holds {} entries for {elements} elements",
            self.cached.len(),
        );
        self.cached[k]
    }
}

/// One class awaiting its `$childSerializers` cache: its id, its INTERNED qualified name, and the
/// serialized properties in element order — everything [`add_child_serializer_cache`] needs on the
/// second pass.
///
/// The name is carried as the interned `TypeName` identity rather than a rendered `String`: a
/// round trip through text would re-intern it, and the only place a class name legitimately
/// becomes characters is the classfile writer.
pub(super) type PendingChildSerializerCache = (ClassId, TypeName, Vec<(String, Ty)>);

/// The `$childSerializers` cache for one `@Serializable` class, built once every OTHER class's
/// `$serializer` exists.
///
/// It must run in a second pass. A slot's element serializer may be a sibling class's
/// `$serializer` singleton, and the classes are generated in an order that is not the source
/// one — so building the cache inside the generation loop asked for a `$serializer` that did
/// not exist yet and silently stored `null` in its place. No single ordering could fix it:
/// two `@Serializable` classes may hold collections of each other.
pub(super) fn add_child_serializer_cache(
    ir: &mut IrFile,
    ctx: &PluginContext,
    class_id: ClassId,
    serialized: TypeName,
    foo_fields: &[(String, Ty)],
) -> Option<ChildSerializerCachePlan> {
    // `$childSerializers` cache — a `private static final Lazy[]` + the public synthetic
    // `access$get$childSerializers$cp()` accessor kotlinc emits when a prop's serializer is
    // ALLOCATED rather than a singleton: a collection (`ArrayListSerializer(…)`) or an ENUM
    // (`EnumSerializer(…)`). A primitive/`String` (singleton `INSTANCE`) or a nested `@Serializable`
    // CLASS (singleton `$$serializer.INSTANCE`) is NOT cached. Each slot is a `Lazy` over its own
    // factory, else null.
    // Which properties need a cached serializer, decided ONCE, before anything is built.
    //
    // A property needs one when its serializer must be ALLOCATED rather than read as a singleton —
    // a collection, an enum. A property that NAMES its serializer answers `false` whatever its
    // type: an explicit `@Serializable(with = X::class)` or a contextual element is built from the
    // DECLARATION, so there is nothing about the type to defer and the cache has no slot for it.
    //
    // Classifying one of those as cacheable asks the type for a serializer it cannot supply, which
    // now REFUSES the file rather than quietly dropping the cache. The classification has to be
    // right, not merely recoverable — and computing it up front is also what lets the build loop
    // below take `ir` mutably.
    let cached_plans: Result<Vec<Option<ElementSerializerPlan>>, UnderivableChildCacheElement> =
        foo_fields
            .iter()
            .map(|(name, ty)| {
                if property_is_contextual(ctx, ir, class_id, name)
                    || field_serializer_of(ctx, ir, class_id, name).is_some()
                {
                    return Ok(None);
                }
                child_cache_element_plan(ir, ctx, ty)
            })
            .collect();

    let cached_plans = match cached_plans {
        Ok(plans) => plans,
        Err(UnderivableChildCacheElement) => {
            let unsupported = ir.add_expr(IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "child-serializer-cache",
                exprs: Vec::new(),
                data: vec![serialized],
                types: Vec::new(),
            });
            ir.statics.push(crate::ir::IrStatic {
                name: "$childSerializers".to_string(),
                ty: lazy_cache_ty(),
                init: unsupported,
                is_var: false,
                is_const: false,
                owner: Some(serialized),
                visibility: crate::types::Visibility::Private,
                setter_jvm_name: None,
                erased_declared_ty: None,
                custom_accessor: true,
                line: 0,
                source_order: u32::MAX,
            });
            return None;
        }
    };

    if cached_plans.iter().all(Option::is_none) {
        return None;
    }
    {
        let lazy_arr_ty = lazy_cache_ty();
        // A slot is `null` ONLY where the selected serializer plan needs no cache. The same plan
        // that made this decision is consumed below, so classification and emission cannot drift
        // into a legacy second derivation.
        let mut elems: Vec<ExprId> = Vec::with_capacity(foo_fields.len());
        let mut factories = 0usize;
        // kotlinc records ONE `LineNumberTable` entry on every member it generates for the cache,
        // and it is the serialized class's annotation-inclusive declaration line — the line its
        // `@Serializable` sits on.
        let owner_decl_line = ir.classes[class_id as usize].decl_start_line;
        for (index, _) in foo_fields.iter().enumerate() {
            let Some(plan) = cached_plans[index].clone() else {
                elems.push(ir.add_expr(IrExpr::Const(IrConst::Null)));
                continue;
            };
            // The point of the cache is that a serializer is built on FIRST USE, so a slot holds a
            // `Lazy` over a FACTORY rather than an already-built serializer. kotlinc emits that
            // factory as a private static synthetic and binds it as a `Function0` through
            // `LambdaMetafactory` — which is what an `IrExpr::Lambda` over that function compiles
            // to. Building eagerly and wrapping with `lazyOf` produces the same values while
            // defeating the deferral the cache exists for.
            //
            // The naming is kotlinc's, measured on a two-slot class: the first factory is bare and
            // the rest are suffixed from zero, so a second one is `…$_anonymous_$0`.
            let factory_name = match factories {
                0 => "_childSerializers$_anonymous_".to_string(),
                n => format!("_childSerializers$_anonymous_${}", n - 1),
            };
            factories += 1;
            // Cache factories have a distinct JVM representation: direct collection operands and
            // the result are narrowed to `KSerializer`. Emit that representation from the selected
            // plan instead of reopening and rewriting generic IR by expression shape.
            let es = emit_cached_element_serializer(ir, plan);
            let returned = ir.add_expr(IrExpr::Return(Some(es)));
            // `implicit_return_end_lines` marks at the RETURN instruction; `expr_lines` would also
            // open the statement at pc 0 and the later mark would collapse into it. kotlinc's
            // factory carries a single entry, on the `areturn`.
            ir.implicit_return_end_lines
                .insert(returned, owner_decl_line);
            let factory_body = ir.add_expr(IrExpr::Block {
                stmts: vec![returned],
                value: None,
            });
            let factory = ir.add_fun(IrFunction {
                name: factory_name,
                params: vec![],
                ret: class_ty(super::KSERIALIZER_FQ),
                body: Some(factory_body),
                is_static: true,
                dispatch_receiver: None,
                param_checks: Vec::new(),
            });
            ir.synthetic_methods.insert(factory);
            ir.serialization_cache_methods.insert(factory);
            // PRIVATE, as kotlinc emits it: nothing outside the class initializer that binds it may
            // call the factory, and publishing it would put a method on the class's ABI that the
            // reference compiler does not have.
            ir.private_methods.insert(factory);
            ir.classes[class_id as usize].methods.push(factory);
            let supplier = ir.add_expr(IrExpr::Lambda {
                impl_fn: factory,
                arity: 0,
                captures: Vec::new(),
                sam: None,
                inline_body: None,
            });
            let mode = ir.add_expr(IrExpr::ExternalStaticField {
                owner: type_name("kotlin/LazyThreadSafetyMode"),
                name: "PUBLICATION".to_string(),
                descriptor: "Lkotlin/LazyThreadSafetyMode;".to_string(),
            });
            elems.push(ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: type_name("kotlin/LazyKt"),
                    name: "lazy".to_string(),
                    descriptor:
                        "(Lkotlin/LazyThreadSafetyMode;Lkotlin/jvm/functions/Function0;)Lkotlin/Lazy;"
                            .to_string(),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![mode, supplier],
            }));
        }
        let arr = ir.add_expr(IrExpr::Vararg {
            array_type: lazy_arr_ty,
            spreads: vec![false; elems.len()],
            elements: elems,
        });
        let static_index = u32::try_from(ir.statics.len()).expect("too many statics");
        ir.statics.push(crate::ir::IrStatic {
            name: "$childSerializers".to_string(),
            ty: lazy_arr_ty,
            init: arr,
            is_var: false,
            is_const: false,
            owner: Some(serialized),
            visibility: crate::types::Visibility::Private,
            setter_jvm_name: None,
            erased_declared_ty: None,
            custom_accessor: true,
            // kotlinc's `<clinit>` maps the cache's array construction to the same declaration line.
            line: owner_decl_line,
            source_order: u32::MAX,
        });
        // Read the static just declared rather than describing the field again: a read built from a
        // DESCRIPTOR carries the erased type, so returning it coerces to the accessor's declared
        // type and emits a `checkcast [Lkotlin/Lazy;` the reference compiler does not. kotlinc's
        // accessor body is `getstatic; areturn`.
        let read = ir.add_expr(IrExpr::GetStatic(static_index));
        let ret = ir.add_expr(IrExpr::Return(Some(read)));
        ir.expr_lines.insert(read, owner_decl_line);
        ir.expr_lines.insert(ret, owner_decl_line);
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let acc = ir.add_fun(IrFunction {
            name: "access$get$childSerializers$cp".to_string(),
            params: vec![],
            ret: lazy_arr_ty,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        ir.synthetic_methods.insert(acc);
        ir.serialization_cache_methods.insert(acc);
        ir.classes[class_id as usize].methods.push(acc);
        Some(ChildSerializerCachePlan {
            static_index,
            accessor: acc,
            cached: cached_plans.iter().map(Option::is_some).collect(),
        })
    }
}

/// The generated `childSerializers()` body: the per-element `KSerializer[]` the runtime reads a
/// class's element serializers out of.
///
/// It lives beside the cache because that is what it now consumes — each cached element is READ
/// from the `Lazy[]` slot the cache pass published rather than rebuilt here, and the method opens
/// by loading that array. The arms for everything else (contextual, type-parameter, an explicitly
/// named serializer, a derived one) are unchanged; they are here so the whole decision of what an
/// element's serializer IS sits in one module instead of straddling the plugin facade.
pub(super) struct ChildSerializersBody<'a> {
    pub(super) function: u32,
    /// The `$serializer` class index — the owner of a type-parameter serializer field.
    pub(super) serializer_class: u32,
    pub(super) serialized_class: ClassId,
    /// The class the PROPERTIES are declared on, for the per-property annotation questions.
    pub(super) declaring_class: ClassId,
    pub(super) fields: &'a [(String, Ty)],
    /// Element types as the SERIALIZER sees them, which is not always the field's own type.
    pub(super) serializer_field_types: &'a [Ty],
    pub(super) type_parameter_serializer_fields: &'a [Option<u32>],
    /// The serialized class's cache plan, or `None` when it has no cache.
    pub(super) plan: Option<ChildSerializerCachePlan>,
}

impl ChildSerializersBody<'_> {
    pub(super) fn generate(self, ir: &mut IrFile, ctx: &PluginContext) {
        let Self {
            function,
            serializer_class,
            serialized_class,
            declaring_class,
            fields,
            serializer_field_types,
            type_parameter_serializer_fields,
            plan,
        } = self;
        // Return the per-field element-serializer array (arity == field count): one
        // `KSerializer` singleton per property. A nested `@Serializable` field uses the
        // krusty-generated `<T>$serializer.INSTANCE`; a directly-supported field uses the
        // builtin `…Serializer.INSTANCE`. An unsupported field type contributes `null`
        // (placeholder) so the array arity still matches the descriptor's element count.
        // The serialized class's cache PLAN, as the pass that built it published
        // it: which elements have a slot, and the accessor to reach them through.
        // kotlinc reads `$childSerializers` once at the top of the method and takes
        // each cached element out of it, rather than rebuilding the serializer the
        // cache already holds. The local is the first free one after the receiver,
        // and the result array lands above it.
        let serialized = ir.classes[serialized_class as usize].fq_name_id();

        let cache_local = plan.as_ref().map(|_| {
            u32::try_from(ir.functions[function as usize].params.len())
                .expect("too many childSerializers parameters")
                + 1
        });
        let elements: Vec<ExprId> = serializer_field_types
            .iter()
            .enumerate()
            .map(|(i, _ty)| {
                if let Some(cached) = cached_element(
                    ir,
                    plan.as_ref(),
                    cache_local,
                    i,
                    serializer_field_types.len(),
                    &serializer_field_types[i],
                ) {
                    cached
                } else if let Some(inst) = super::contextual_serializer_for(
                    ir,
                    property_is_contextual(ctx, ir, declaring_class, &fields[i].0),
                    &serializer_field_types[i],
                ) {
                    // `@Contextual` / file-level `@UseContextualSerialization` property.
                    inst
                } else if let Some(fidx) = type_parameter_serializer_fields[i] {
                    // Type-parameter element: `this.typeSerialK` (the ctor-supplied serializer).
                    let this = ir.add_expr(IrExpr::GetValue(0));
                    ir.add_expr(IrExpr::GetField {
                        receiver: this,
                        class: serializer_class,
                        index: fidx,
                    })
                } else if let Some(internal) =
                    field_serializer_of(ctx, ir, declaring_class, &fields[i].0)
                {
                    // Explicit per-property serializer: `new X()` (or `X.INSTANCE`),
                    // wrapped `.nullable` for a nullable property.
                    let base = super::build_field_serializer_instance(ir, internal);
                    if super::is_nullable(&serializer_field_types[i]) {
                        super::wrap_nullable_serializer(ir, base)
                    } else {
                        base
                    }
                } else if let Some(e) = super::element_serializer_expr(
                    ir,
                    ctx,
                    &serializer_field_types[i],
                )
                .map(|base| {
                    // A NULLABLE property's element serializer is the base one wrapped
                    // `.nullable`, narrowed to the `KSerializer` that wrapper takes. Only the
                    // explicit-serializer arm above did this, so every nullable property whose
                    // serializer is DERIVED from its type published the non-null serializer.
                    if super::is_nullable(&serializer_field_types[i]) {
                        let narrowed =
                            super::deserialize_body::narrowed(ir, base, super::KSERIALIZER_FQ);
                        super::wrap_nullable_serializer(ir, narrowed)
                    } else {
                        base
                    }
                }) {
                    // Nested @Serializable (generic `Foo<A>` → `Foo.serializer(A_ser)`,
                    // or non-generic `Foo$serializer.INSTANCE`) | builtin `…Serializer`.
                    e
                } else {
                    ir.add_expr(IrExpr::Const(IrConst::Null))
                }
            })
            .collect();
        let arr = ir.add_expr(IrExpr::Vararg {
            array_type: Ty::obj_args("kotlin/Array", &[class_ty(super::KSERIALIZER_FQ)]),
            spreads: vec![false; elements.len()],
            elements,
        });
        let ret = ir.add_expr(IrExpr::Return(Some(arr)));
        let mut stmts = Vec::with_capacity(2);
        if let (Some(local), Some(plan)) = (cache_local, plan.as_ref()) {
            stmts.push(load_cache_statement(ir, plan, local, serialized));
        }
        stmts.push(ret);
        let body = ir.add_expr(IrExpr::Block { stmts, value: None });
        ir.functions[function as usize].body = Some(body);
    }
}

#[cfg(test)]
mod tests {
    use super::add_child_serializer_cache;
    use crate::ir::{IrExpr, IrFile};
    use crate::plugins::{synthetic_class, PluginContext};
    use crate::types::Ty;

    use super::class_ty;

    fn holder(element: Ty) -> (IrFile, crate::ir::ClassId, Vec<(String, Ty)>) {
        let mut ir = IrFile::default();
        let mut holder = synthetic_class("demo/Holder");
        holder.fields = vec![crate::ir::IrField::new("tags".to_string(), element)];
        holder.ctor_param_count = 1;
        let class_id = ir.add_class(holder);
        (ir, class_id, vec![("tags".to_string(), element)])
    }

    /// A cached slot whose serializer cannot be constructed REFUSES the file.
    ///
    /// `List<demo/Unknown>` needs a cache — it is a collection — and `Unknown` has no `$serializer`
    /// in the arena, so no element serializer can be built for it. Dropping the cache and letting
    /// every use build its own would be a second lowering deciding the compilation, so the plugin
    /// publishes an unsupported residual and `jvm_can_emit` declines instead.
    ///
    /// Driven directly rather than through a source fixture: reproducing this needs a shape the
    /// REFERENCE compiler accepts and krusty cannot derive, which is a moving target, while the
    /// invariant is not.
    #[test]
    fn an_underivable_cached_slot_refuses_the_file() {
        let element = Ty::obj_args("kotlin/collections/List", &[class_ty("demo/Unknown")]);
        let (mut ir, class_id, fields) = holder(element);
        let serialized = ir.classes[class_id as usize].fq_name_id();

        add_child_serializer_cache(
            &mut ir,
            &PluginContext::default(),
            class_id,
            serialized,
            &fields,
        );

        let cache = ir
            .statics
            .iter()
            .find(|s| s.name == "$childSerializers")
            .expect("the refusal is published as the cache's own initializer");
        assert!(
            matches!(
                ir.expr(cache.init),
                IrExpr::PluginPlaceholder {
                    plugin: "serialization",
                    ..
                }
            ),
            "an underivable required slot publishes an unsupported residual, not a null"
        );
        assert!(
            !crate::jvm::ir_emit::jvm_can_emit(&ir),
            "and that residual must make the file undecidable rather than emit a broken cache"
        );
    }

    /// The companion case: every cached slot derives, so the cache is built and the file emits.
    #[test]
    fn a_derivable_cached_slot_builds_the_cache() {
        let element = Ty::obj_args("kotlin/collections/List", &[Ty::String]);
        let (mut ir, class_id, fields) = holder(element);
        let serialized = ir.classes[class_id as usize].fq_name_id();

        add_child_serializer_cache(
            &mut ir,
            &PluginContext::default(),
            class_id,
            serialized,
            &fields,
        );

        let cache = ir
            .statics
            .iter()
            .find(|s| s.name == "$childSerializers")
            .expect("a `List<String>` element serializer derives, so the cache is built");
        assert!(
            !matches!(ir.expr(cache.init), IrExpr::PluginPlaceholder { .. }),
            "a derivable slot publishes a real initializer"
        );
        assert!(
            ir.classes[class_id as usize]
                .methods
                .iter()
                .any(|&m| ir.functions[m as usize].name == "access$get$childSerializers$cp"),
            "and the accessor the `$serializer` reads it through"
        );
    }
}
