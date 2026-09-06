use super::*;

fn semantic_references(ir: &IrFile) -> Vec<&crate::ir::IrCallableReference> {
    ir.exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::CallableReference(reference) => Some(reference),
            _ => None,
        })
        .collect()
}

fn assert_common_ir_has_no_jvm_reference_carrier(ir: &IrFile) {
    assert!(
        ir.classes.iter().all(|class| class.func_ref.is_none()),
        "common lowering must not synthesize a backend callable-reference carrier"
    );
    assert!(
        ir.classes.iter().all(|class| !class
            .superclass
            .matches("kotlin/jvm/internal/FunctionReferenceImpl")
            && !class
                .superclass
                .matches("kotlin/jvm/internal/AdaptedFunctionReference")),
        "common lowering must not name a JVM callable-reference implementation"
    );
}

#[test]
fn module_callable_reference_stays_semantic_through_common_lowering() {
    let ir = lower_single_source(
        "fun selected(value: Int): Int = value\nfun reference(): (Int) -> Int = ::selected\n",
        "ModuleReference",
    );

    let references = semantic_references(&ir);
    let [reference] = references.as_slice() else {
        panic!("one semantic callable-reference value")
    };
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::Module(_)
    ));
    assert!(reference.captures.is_empty());
    assert_common_ir_has_no_jvm_reference_carrier(&ir);
}

#[test]
fn unadapted_local_function_reference_keeps_structural_identity_and_capture() {
    let ir = lower_single_source(
        r#"
            fun apply(value: Int, operation: (Int) -> Int): Int = operation(value)
            fun caller(): Int {
                val base = 40
                fun add(value: Int): Int = base + value
                return apply(2, ::add)
            }
        "#,
        "LocalReference",
    );

    let references = semantic_references(&ir);
    let [reference] = references.as_slice() else {
        panic!("one semantic local function reference")
    };
    assert_eq!(reference.captures.len(), 1);
    assert!(matches!(
        &reference.target,
        crate::ir::IrCallableReferenceTarget::Local { name, .. } if name.as_ref() == "add"
    ));
    assert!(matches!(
        reference.function_type,
        Ty::Fun(signature) if signature.params.len() == 1
    ));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_local_function_reference_becomes_default_call_wrapper() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String) -> String {
                fun join(value: String, suffix: String = "K"): String = value + suffix
                return ::join
            }
        "#,
        "AdaptedLocalReference",
    );

    let join = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("join$fir_"))
        .expect("lifted local target") as u32;
    assert!(ir.functions.iter().any(|function| {
        function.name.starts_with("$fir_local_ref_")
            && function.params == [Ty::String]
            && function.ret == Ty::String
    }));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::LocalWithDefaults {
                function,
                defaults,
            },
            args,
            ..
        } if *function == join
            && defaults.as_ref() == [1]
            && args.len() == 1
    )));
    assert!(semantic_references(&ir).iter().any(|reference| {
        matches!(
            reference.target,
            crate::ir::IrCallableReferenceTarget::Local { .. }
        ) && reference.adaptation.is_some()
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.len() == 1)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_local_reference_becomes_suspend_wrapper() {
    let ir = lower_single_source(
        r#"
            fun outer(): suspend (Int) -> Int {
                fun increment(value: Int): Int = value + 1
                return ::increment
            }
        "#,
        "SuspendConvertedLocalReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_local_ref_"))
        .expect("suspend conversion wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::Int]);
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::Int);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(semantic_references(&ir).iter().any(|reference| {
        reference.adapter == wrapper
            && reference.captures.is_empty()
            && matches!(reference.function_type, Ty::Fun(signature)
                if signature.suspend && signature.params.len() == 1)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_function_value_becomes_checked_forwarding_wrapper() {
    let ir = lower_single_source(
        r#"
            fun convert(block: (Int) -> String): suspend (Int) -> String = block
        "#,
        "SuspendConvertedFunctionValue",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_suspend_delegate_"))
        .expect("suspend function-value conversion wrapper") as u32;
    assert!(matches!(
        ir.functions[wrapper as usize].params.as_slice(),
        [Ty::Fun(source), Ty::Int]
            if !source.suspend && source.params == [Ty::Int] && source.ret == Ty::String
    ));
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::String);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 2,
            captures,
            sam: None,
            ..
        } if *impl_fn == wrapper && captures.len() == 1
    )));
}

#[test]
fn suspend_converted_source_reference_becomes_suspend_wrapper() {
    let ir = lower_single_source(
        r#"
            fun increment(value: Int): Int = value + 1
            fun outer(): suspend (Int) -> Int = ::increment
        "#,
        "SuspendConvertedSourceReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("suspend conversion wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::Int]);
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::Int);
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn direct_suspend_source_reference_becomes_suspend_wrapper() {
    let ir = lower_source_from_set(
        &[
            (
                "suspend inline fun Int.selected(value: Int): Int = this + value",
                "Dependency",
            ),
            (
                "fun reference(): suspend (Int) -> Int = 1::selected",
                "Consumer",
            ),
        ],
        1,
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("direct suspend reference wrapper") as u32;
    assert!(ir.suspend_funs.contains(&wrapper));
}

#[test]
fn function_invoke_reference_becomes_a_capturing_suspend_forwarder() {
    let ir = lower_single_source(
        r#"
            fun reference(block: suspend () -> Unit): suspend () -> Unit = block::invoke
        "#,
        "FunctionInvokeReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_invoke_ref_"))
        .expect("function invoke forwarding wrapper") as u32;
    assert_eq!(ir.functions[wrapper as usize].params.len(), 1);
    assert!(matches!(
        ir.functions[wrapper as usize].params[0],
        Ty::Fun(signature) if signature.suspend && signature.params.is_empty()
    ));
    assert_eq!(ir.functions[wrapper as usize].ret, Ty::obj("kotlin/Unit"));
    assert!(ir.suspend_funs.contains(&wrapper));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            ..
        } if *impl_fn == wrapper && captures.len() == 1
    )));
    assert!(ir.exprs.iter().enumerate().any(|(expression, node)| {
        matches!(node, IrExpr::InvokeFunction { args, params, ret, .. }
            if args.is_empty() && params.is_empty() && *ret == Ty::Unit)
            && ir.suspend_calls.get(&(expression as u32)) == Some(&Ty::Unit)
    }));
}

#[test]
fn vararg_adapted_local_reference_packs_wrapper_parameters() {
    let ir = lower_single_source(
        r#"
            fun outer(): (Int, Int) -> Int {
                fun sum(vararg values: Int): Int = values[0] + values[1]
                return ::sum
            }
        "#,
        "AdaptedLocalVarargReference",
    );

    let sum = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("sum$fir_"))
        .expect("lifted local vararg target") as u32;
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            ..
        } if *function == sum
            && matches!(args.as_slice(), [array]
                if matches!(ir.expr(*array), IrExpr::Vararg { elements, .. } if elements.len() == 2))
    )));
    assert!(semantic_references(&ir).iter().any(|reference| {
        matches!(
            reference.target,
            crate::ir::IrCallableReferenceTarget::Local { .. }
        ) && reference.adaptation.is_some()
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.len() == 2)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn unbound_local_extension_reference_keeps_receiver_as_structural_parameter() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String, String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return String::tag
            }
        "#,
        "LocalExtensionReference",
    );

    assert!(semantic_references(&ir).iter().any(|reference| {
        reference.captures.is_empty()
            && matches!(
                reference.target,
                crate::ir::IrCallableReferenceTarget::Local { .. }
            )
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.len() == 2)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn bound_local_extension_reference_captures_selected_receiver() {
    let ir = lower_single_source(
        r#"
            fun outer(): (String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return "O"::tag
            }
        "#,
        "BoundLocalExtensionReference",
    );

    assert!(semantic_references(&ir).iter().any(|reference| {
        reference.captures.is_empty()
            && reference.bound_receiver.is_some()
            && matches!(
                reference.target,
                crate::ir::IrCallableReferenceTarget::Local { .. }
            )
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.len() == 1)
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_local_extension_references_materialize_bound_and_unbound_receivers() {
    let ir = lower_single_source(
        r#"
            fun unbound(): (String) -> String {
                fun String.tag(suffix: String = "K"): String = this + suffix
                return String::tag
            }
            fun bound(): () -> String {
                fun String.tag(suffix: String = "K"): String = this + suffix
                return "O"::tag
            }
        "#,
        "AdaptedLocalExtensionReferences",
    );

    let references = semantic_references(&ir);
    assert!(references.iter().any(|reference| {
        reference.captures.is_empty()
            && reference.bound_receiver.is_none()
            && reference.adaptation.is_some()
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.len() == 1)
    }));
    assert!(references.iter().any(|reference| {
        reference.captures.is_empty()
            && reference.bound_receiver.is_some()
            && reference.adaptation.is_some()
            && matches!(reference.function_type, Ty::Fun(signature) if signature.params.is_empty())
    }));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn suspend_converted_local_extension_references_become_suspend_wrappers() {
    let ir = lower_single_source(
        r#"
            fun unbound(): suspend (String, String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return String::tag
            }
            fun bound(): suspend (String) -> String {
                fun String.tag(suffix: String): String = this + suffix
                return "O"::tag
            }
        "#,
        "SuspendLocalExtensionReferences",
    );

    let wrappers = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.name.starts_with("$fir_local_ref_"))
        .map(|(function, _)| function as u32)
        .collect::<Vec<_>>();
    assert_eq!(wrappers.len(), 2);
    assert!(wrappers
        .iter()
        .all(|wrapper| ir.suspend_funs.contains(wrapper)));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn source_callable_references_become_structural_values_or_adapters() {
    let ir = lower_single_source(
        r#"
            fun increment(value: Int): Int = value + 1
            fun withDefault(value: Int, amount: Int = 1): Int = value + amount
            fun sum(vararg values: Int): Int = values[0] + values[1]
            fun <T> identity(value: T): T = value
            fun String.append(value: String): String = this + value
            fun String.appendDefault(value: String = "K"): String = this + value
            class Counter { fun increment(value: Int): Int = value + 1 }
            fun references(counter: Counter) {
                val topLevel = ::increment
                val bound = counter::increment
                val unbound = Counter::increment
                val defaulted: (Int) -> Int = ::withDefault
                val packed: (Int, Int) -> Int = ::sum
                val generic: (String) -> String = ::identity
                val boundExtension = "a"::append
                val unboundExtension = String::append
                val boundDefaultExtension: () -> String = "O"::appendDefault
                val unboundDefaultExtension: (String) -> String = String::appendDefault
                topLevel(1)
                bound(1)
                unbound(counter, 1)
                defaulted(1)
                packed(1, 2)
                generic("value")
                boundExtension("b")
                unboundExtension("a", "b")
                boundDefaultExtension()
                unboundDefaultExtension("O")
            }
        "#,
        "SourceReferences",
    );

    let adapters = ir
        .exprs
        .iter()
        .filter(|expression| matches!(expression, IrExpr::Lambda { .. }))
        .count();
    let structural_references = semantic_references(&ir).len();
    assert_eq!(adapters + structural_references, 10);
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn adapted_reference_arguments_keep_checked_target_coercions() {
    let ir = lower_single_source(
        r#"
            fun collect(vararg values: Any) {}
            fun consume(value: Any, tail: Int = 0) {}
            fun references() {
                val packed: (Int, Int) -> Unit = ::collect
                val widened: (Int) -> Unit = ::consume
            }
        "#,
        "AdaptedReferenceCoercions",
    );

    let packed = ir
        .exprs
        .iter()
        .find_map(|expression| match expression {
            IrExpr::Vararg {
                array_type,
                elements,
                ..
            } if array_type.array_elem() == Some(Ty::obj("kotlin/Any")) => Some(elements),
            _ => None,
        })
        .expect("collected Any vararg");
    assert_eq!(packed.len(), 2);
    assert!(packed.iter().all(|element| matches!(
        ir.expr(*element),
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } if *type_operand == Ty::obj("kotlin/Any")
    )));
    assert_eq!(
        ir.exprs
            .iter()
            .filter(|expression| matches!(
                expression,
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    type_operand,
                    ..
                } if *type_operand == Ty::obj("kotlin/Any")
            ))
            .count(),
        3,
        "two collected values and one ordinary adapted parameter cross the checked Any boundary"
    );
}

#[test]
fn unbound_extension_reference_keeps_checked_receiver_coercion() {
    let ir = lower_single_source(
        r#"
            fun Any.describe(): String = "OK"
            fun reference(): Int.() -> String = Any::describe
        "#,
        "UnboundExtensionReferenceReceiverCoercion",
    );

    assert_eq!(semantic_references(&ir).len(), 1);
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } if *type_operand == Ty::obj("kotlin/Any")
    )));
}

#[test]
fn unadapted_reference_argument_keeps_checked_parameter_coercion() {
    let ir = lower_single_source(
        r#"
            fun consume(value: Any): String = "OK"
            fun reference(): (Int) -> String = ::consume
        "#,
        "UnadaptedReferenceParameterCoercion",
    );

    assert_eq!(semantic_references(&ir).len(), 1);
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } if *type_operand == Ty::obj("kotlin/Any")
    )));
}

#[test]
fn adapted_generic_extension_reference_consumes_published_substitution() {
    let ir = lower_single_source(
        r#"
            fun <T> T.generic(vararg values: String, count: Int = 0): Int = count
            fun reference(): Int.(Array<String>, Int) -> Unit = Int::generic
        "#,
        "GenericExtensionReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_callable_ref_"))
        .expect("adapted generic extension-reference wrapper") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [
            Ty::Int,
            Ty::obj_args("kotlin/Array", &[Ty::String]),
            Ty::Int,
        ]
    );
    assert!(semantic_references(&ir).iter().any(|reference| {
        reference.adapter == wrapper
            && matches!(
                reference.target,
                crate::ir::IrCallableReferenceTarget::Module(_)
            )
    }));
    assert!(ir.exprs.iter().all(|expression| {
        !matches!(
            expression,
            IrExpr::Checked(IrCheckedOperation::CallableReference { .. })
        )
    }));
}

#[test]
fn size_only_primitive_array_constructor_reference_lowers_mechanically() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "private fun <T> upcast(value: T): T = value\n\
         fun use() { upcast<(Int) -> ByteArray>(::ByteArray)(10) }\n",
        "SizeOnlyPrimitiveArrayReference",
        platform,
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::NewArray { array_type, .. } if *array_type == Ty::obj("kotlin/ByteArray")
    )));
    assert!(ir
        .exprs
        .iter()
        .all(|expression| !matches!(expression, IrExpr::Checked(_))));
}

#[test]
fn inline_array_initializer_is_spliced_before_common_ir_escapes() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "fun objectArray() { Array<String>(2) { i -> if (i == 1) return; i.toString() } }\n\
         fun primitiveArray() { IntArray(2) { i -> if (i == 1) return; i } }\n",
        "InlineArrayInitializer",
        platform,
    );

    assert_eq!(ir.inline_only_fns.len(), 2);
    assert!(ir.inline_only_fns.iter().all(|function| {
        ir.functions
            .get(*function as usize)
            .is_some_and(|function| function.body.is_none())
    }));
    assert!(ir.exprs.iter().all(|expression| !matches!(
        expression,
        IrExpr::InvokeFunction { func, .. }
            if matches!(ir.expr(*func), IrExpr::Lambda { inline_body: Some(_), .. })
    )));
}

#[test]
fn suspending_collection_map_is_structural_before_common_ir_escapes() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_STDLIB"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "suspend fun render(value: Int): Int = value + 1\n\
         suspend fun collect(values: List<Int>): List<Int> =\n\
             values.map { render(it) }\n",
        "SuspendCollectionMap",
        platform,
    );
    let collect = ir
        .functions
        .iter()
        .enumerate()
        .find_map(|(function, declaration)| {
            (declaration.name == "collect").then_some(function as u32)
        })
        .expect("collect common-IR function");
    let body = ir.functions[collect as usize]
        .body
        .expect("collect body must be streamed");
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut has_loop = false;
    let mut has_suspend_call = false;
    let mut has_lambda = false;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        has_loop |= matches!(ir.expr(expression), IrExpr::While { .. });
        has_suspend_call |= ir.suspend_calls.contains_key(&expression);
        has_lambda |= matches!(ir.expr(expression), IrExpr::Lambda { .. });
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert!(
        has_loop,
        "selected map body must already be an ordinary IR loop"
    );
    assert!(
        has_suspend_call,
        "the lambda's checked suspend call must belong to collect before target lowering"
    );
    assert!(
        !has_lambda,
        "a suspending inline map body cannot escape as a non-suspend Function1"
    );
}

#[test]
fn super_call_preserves_checked_default_ordinals_and_source_identity() {
    let ir = lower_single_source(
        "open class Base {\n\
             open fun value(x: Int = 20, y: Int = 3): Int = x + y\n\
         }\n\
         class Derived : Base() {\n\
             fun read(): Int = super.value(y = 4)\n\
         }\n",
        "SuperCallDefaults",
    );

    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Super {
                source: Some(_),
                realization: crate::libraries::MemberRealization::Dispatch,
                defaults,
                ..
            },
            args,
            ..
        } if defaults == &[0] && args.len() == 2
    )));
}

#[test]
fn sibling_inline_adapted_reference_uses_the_retained_inline_template() {
    let ir = lower_source_from_set(
        &[
            ("inline fun sibling(value: Int): Int = value", "Dependency"),
            ("fun reference(): (Int) -> Unit = ::sibling", "Caller"),
        ],
        1,
    );

    assert!(ir.functions.iter().any(|function| {
        function.name.starts_with("$fir_callable_ref_") && function.params == [Ty::Int]
    }));
    assert!(!ir.foreign_inline_templates.is_empty());
    assert!(!ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Module {
                name,
                ..
            },
            dispatch_receiver: None,
            ..
        } if name == "sibling"
    )));
}

#[test]
fn sibling_suspend_call_retains_its_checked_coroutine_behavior() {
    let ir = lower_source_from_set(
        &[
            ("suspend fun sibling(): Int = 1", "Dependency"),
            ("suspend fun caller(): Int = sibling()", "Caller"),
        ],
        1,
    );

    let (expression, _) = ir
        .exprs
        .iter()
        .enumerate()
        .find(|(_, expression)| {
            matches!(
                expression,
                IrExpr::Call {
                    callee: crate::ir::Callee::Module { name, .. },
                    ..
                } if name == "sibling"
            )
        })
        .expect("sibling suspend call");
    assert_eq!(ir.suspend_calls.get(&(expression as u32)), Some(&Ty::Int));
}

#[test]
fn suspend_inline_call_keeps_source_extent_and_semantic_inline_region() {
    let source = "// WITH_STDLIB\n\
                  suspend fun values(): List<Int> = listOf(1)\n\
                  suspend fun work(): List<Int> =\n\
                  \x20 values().filter { it > 0 }\n";
    let mut classpath = crate::toolchain::classpath_jars_for("// WITH_STDLIB\n// WITH_REFLECT");
    if let Some(jdk) = crate::toolchain::jdk_modules() {
        classpath.push(jdk);
    }
    let ir = lower_single_source_with_platform(
        source,
        "SuspendInlineRegion",
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(classpath)),
        )),
    );

    assert_eq!(
        ir.source_line_count,
        source.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1
    );
    assert_eq!(ir.suspend_calls.len(), 1);
    let suspension = *ir.suspend_calls.keys().next().unwrap();
    assert_eq!(ir.expr_source_lines.get(&suspension), Some(&4));
    assert!(!ir.inline_call_sites.is_empty());
    assert!(!ir.inline_regions.is_empty());
}

#[test]
fn block_trailing_expression_keeps_its_checked_statement_line() {
    let ir = lower_single_source(
        "fun cond(): Boolean = false\n\
         fun act() {}\n\
         fun loop() {\n\
             while (cond()) {\n\
                 act()\n\
             }\n\
         }\n",
        "BlockTrailingLine",
    );
    let call = ir
        .exprs
        .iter()
        .enumerate()
        .find_map(|(expression, node)| {
            let IrExpr::Call {
                callee: Callee::Local(function),
                ..
            } = node
            else {
                return None;
            };
            (ir.functions[*function as usize].name == "act").then_some(expression as u32)
        })
        .expect("act call");
    assert_eq!(ir.expr_lines.get(&call), Some(&5));
}

#[test]
fn companion_extension_references_lower_as_receiverless_static_values() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        "class C\n\
         companion fun C.function(): String = \"OK\"\n\
         companion val C.property: String = \"OK\"\n\
         fun references() {\n\
             val function: () -> String = C::function\n\
             val property: () -> String = C::property\n\
             function()\n\
             property()\n\
         }\n",
        "CompanionExtensionReferences",
        platform,
    );

    let receiverless_references = ir
        .exprs
        .iter()
        .filter(|expression| {
            matches!(
                expression,
                IrExpr::Lambda {
                    arity: 0,
                    captures,
                    ..
                } if captures.is_empty()
            )
        })
        .count();
    assert!(receiverless_references >= 1);
    assert!(ir.exprs.iter().all(|expression| {
        !matches!(
            expression,
            IrExpr::Checked(
                IrCheckedOperation::CallableReference { .. }
                    | IrCheckedOperation::PropertyReference { .. }
            )
        )
    }));
}

#[test]
fn companion_associated_function_body_and_call_have_no_runtime_receiver_slot() {
    let ir = lower_single_source(
        "class C\n\
         companion fun C.echo(value: String): String = value\n\
         fun box(): String = C.echo(\"OK\")\n",
        "CompanionAssociatedCall",
    );

    let echo = ir
        .functions
        .iter()
        .position(|function| function.name == "echo")
        .expect("associated function") as u32;
    assert_eq!(ir.functions[echo as usize].params, [Ty::String]);
    assert!(!ir.extension_receiver_fns.contains(&echo));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Call {
            callee: crate::ir::Callee::Local(function),
            args,
            dispatch_receiver: None,
        } if *function == echo && args.len() == 1
    )));
}

#[test]
fn implicit_inner_constructor_reference_captures_the_checked_outer_receiver() {
    let ir = lower_single_source(
        r#"
            class Outer {
                inner class Inner(val value: Int)
                fun reference(): (Int) -> Inner = ::Inner
            }
        "#,
        "BoundInnerConstructorReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("inner-constructor adapter") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [Ty::obj("Outer"), Ty::Int]
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            sam: None,
            inline_body: None,
        } if *impl_fn == wrapper && captures.len() == 1
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::New { internal, args, .. }
            if internal.matches("Outer$Inner") && args.len() == 2
    )));
}

#[test]
fn ordinary_inner_constructor_keeps_source_property_after_physical_outer_prefix() {
    let ir = lower_single_source(
        r#"
            class Outer(val prefix: String) {
                inner class Inner(val value: String) {
                    fun result(): String = prefix + value
                }
                fun make(): Inner = Inner("OK")
            }
        "#,
        "InnerConstructorProperty",
    );

    let inner = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Outer$Inner"))
        .expect("inner class");
    assert_eq!(inner.constructor_prefix_count, 1);
    assert_eq!(inner.ctor_args.len(), 2);
    assert_eq!(inner.ctor_args[1].name.as_deref(), Some("value"));
    assert!(inner.ctor_args[1].is_field);
}

#[test]
fn inner_property_initializer_offsets_source_parameters_after_enclosing_prefix() {
    let ir = lower_single_source(
        r#"
            class Outer(val prefix: String) {
                inner class Inner(val suffix: String) {
                    val value = prefix + suffix
                }
            }
        "#,
        "InnerInitializerParameter",
    );

    let inner = ir
        .classes
        .iter()
        .find(|class| class.fq_name.matches("Outer$Inner"))
        .expect("inner class");
    let body = inner.init_body.expect("property initializer");
    let mut pending = vec![body];
    let mut seen = std::collections::HashSet::new();
    let mut reads_source_suffix = false;
    let mut reads_enclosing_prefix_as_source = false;
    while let Some(expression) = pending.pop() {
        if !seen.insert(expression) {
            continue;
        }
        match ir.expr(expression) {
            IrExpr::GetValue(2) => reads_source_suffix = true,
            IrExpr::GetValue(1) => reads_enclosing_prefix_as_source = true,
            _ => {}
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
    assert!(reads_source_suffix);
    assert!(!reads_enclosing_prefix_as_source);
}

#[test]
fn structural_nested_constructor_reference_retains_its_adapter_identity() {
    let ir = lower_single_source(
        r#"
            class A {
                class Nested(val value: String)
            }
            fun reference(): (String) -> A.Nested = A::Nested
        "#,
        "NestedConstructorReference",
    );

    let adapter = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("constructor-reference adapter") as u32;
    let reference = semantic_references(&ir)
        .into_iter()
        .find(|reference| {
            matches!(
                reference.target,
                crate::ir::IrCallableReferenceTarget::Constructor { .. }
            )
        })
        .expect("semantic constructor reference");
    assert_eq!(reference.adapter, adapter);
    assert!(ir.private_methods.contains(&adapter));
}

#[test]
fn inner_super_constructor_uses_the_checked_constructor_prefix_receiver() {
    let ir = lower_single_source(
        r#"
            open class Outer {
                open inner class Base
            }
            fun make() {
                val derived = object : Outer() {
                    inner class Derived : Base()
                }
                derived.Derived()
            }
        "#,
        "InnerSuperOuter",
    );

    let derived = ir
        .classes
        .iter()
        .find(|class| class.superclass.matches("Outer$Base"))
        .expect("inner Derived class");
    assert_eq!(derived.super_ctor_params, [Ty::obj("Outer")]);
    assert_eq!(derived.super_args.len(), 1);
    assert!(matches!(
        ir.expr(derived.super_args[0]),
        IrExpr::GetValue(1)
    ));
}

#[test]
fn external_inner_constructor_prepends_the_checked_outer_receiver() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(17);
    let mut body = FirBody::new(BodyOwnerId::from_raw(3));
    let outer = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("dependency/Outer")),
        kind: FirExprKind::Constant(FirConstant::Null),
    });
    let construction = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj("dependency/Outer$Inner")),
        kind: FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::External {
                declaration,
                classifier: crate::types::type_name("dependency/Outer$Inner"),
                parameters: Box::new([]),
                annotation: None,
            },
            context_parameter_count: 0,
            outer_parameter: Some(resolved(Ty::obj("dependency/Outer"))),
            outer_receiver: Some(FirReceiver {
                value: outer,
                conversion: None,
            }),
            external_capture_arguments: None,
            parameter_types: Box::new([]),
            arguments: Box::new([]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(construction),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    assert!(matches!(
        ir.expr(lowered.roots[0]),
        IrExpr::New {
            internal,
            args,
            ctor_params: Some(parameters),
            ctor_desc: None,
            external_target: Some(target),
            defaults,
            default_prefix_count,
        } if internal.matches("dependency/Outer$Inner")
            && args.as_slice() == [0]
            && parameters.as_slice() == [Ty::obj("dependency/Outer")]
            && *target == declaration
            && defaults.is_empty()
            && *default_prefix_count == 1
    ));
}

#[test]
fn external_annotation_construction_reaches_common_ir_without_provider_lookup() {
    let origin = OriginId::from_raw(0);
    let declaration = ExternalCallableId::from_raw(19);
    let interface = crate::types::type_name("dependency/Holder$Marker");
    let mut body = FirBody::new(BodyOwnerId::from_raw(4));
    let value = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::String),
        kind: FirExprKind::Constant(FirConstant::String("OK".into())),
    });
    let construction = body.add_expr(FirExpr {
        origin,
        ty: resolved(Ty::obj_name(interface)),
        kind: FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::External {
                declaration,
                classifier: interface,
                parameters: Box::new([resolved(Ty::String)]),
                annotation: Some(Box::new(FirAnnotationConstruction {
                    members: Box::new([("name".into(), resolved(Ty::String))]),
                    defaults: Box::new([None]),
                })),
            },
            context_parameter_count: 0,
            outer_parameter: None,
            outer_receiver: None,
            external_capture_arguments: None,
            parameter_types: Box::new([resolved(Ty::String)]),
            arguments: Box::new([FirCallArgument::Expression {
                parameter: 0,
                value,
                conversion: None,
            }]),
            substitutions: Box::new([]),
        }),
    });
    let statement = body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::Expression(construction),
    });
    body.push_root(statement);

    let mut ir = IrFile::default();
    let lowered = lower_body(body, &ResolvedModuleIndex::default(), &mut ir).unwrap();
    let construction = lowered.roots[0];
    assert!(matches!(
        ir.expr(construction),
        IrExpr::New {
            internal,
            external_target: Some(target),
            defaults,
            default_prefix_count,
            ..
        } if *internal == interface
            && *target == declaration
            && defaults.is_empty()
            && *default_prefix_count == 0
    ));
    let annotation = ir
        .annotation_constructions
        .get(&construction)
        .expect("checked annotation plan");
    assert_eq!(annotation.interface, interface);
    assert_eq!(annotation.members, vec![("name".to_string(), Ty::String)]);
    assert_eq!(annotation.defaults, vec![None]);
}

#[test]
fn unbound_inner_constructor_reference_maps_outer_and_vararg_parameters() {
    let ir = lower_single_source(
        r#"
            class Outer {
                inner class Inner(val value: Int, vararg val rest: String)
            }
            fun reference(): (Outer, Int, String) -> Outer.Inner = Outer::Inner
        "#,
        "UnboundInnerConstructorReference",
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("adapted inner-constructor adapter") as u32;
    assert_eq!(
        ir.functions[wrapper as usize].params,
        [Ty::obj("Outer"), Ty::Int, Ty::String]
    );
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 3,
            captures,
            ..
        } if *impl_fn == wrapper && captures.is_empty()
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::New { internal, args, .. }
            if internal.matches("Outer$Inner")
                && args.len() == 3
                && matches!(ir.expr(args[2]), IrExpr::Vararg { elements, .. } if elements.len() == 1)
    )));
}

#[test]
fn reflective_unbound_inner_constructor_reference_keeps_structural_identity() {
    let platform: Box<dyn crate::libraries::SemanticPlatform> =
        Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(
            std::rc::Rc::new(crate::jvm::classpath::Classpath::new(
                crate::toolchain::classpath_jars_for("// WITH_REFLECT"),
            )),
        ));
    let ir = lower_single_source_with_platform(
        r#"
            import kotlin.reflect.KFunction1
            class Outer { inner class Inner }
            fun reference(): KFunction1<Outer, Outer.Inner> = Outer::Inner
        "#,
        "ReflectiveInnerConstructorReference",
        platform,
    );

    let wrapper = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_ctor_ref_"))
        .expect("inner-constructor adapter") as u32;
    assert_eq!(ir.functions[wrapper as usize].params, [Ty::obj("Outer")]);
    let reference = semantic_references(&ir)
        .into_iter()
        .find(|reference| {
            matches!(
                reference.target,
                crate::ir::IrCallableReferenceTarget::Constructor { .. }
            )
        })
        .expect("semantic reflective constructor reference");
    assert_eq!(reference.adapter, wrapper);
    assert!(reference.captures.is_empty());
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::Constructor { classifier }
            if classifier == crate::types::type_name("Outer$Inner")
    ));
    assert!(matches!(reference.function_type, Ty::Fun(signature)
        if signature.params == [Ty::obj("Outer")]));
    assert_eq!(
        reference.declaration_parameters.as_ref(),
        [Ty::obj("Outer")]
    );
    assert_eq!(reference.declaration_result, Ty::Unit);
}

#[test]
fn consuming_lowering_attaches_selected_sam_target_to_lambda() {
    let ir = lower_single_source(
        "fun interface Action { fun run(value: Int): String }\n\
         fun consume(action: Action): String = \"OK\"\n\
         fun make(): String = consume { value -> \"$value\" }\n",
        "SamConversion",
    );

    let conversions = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                sam: Some(target), ..
            } => Some(target),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(conversions.len(), 1);
    assert_eq!(conversions[0].method, "run");
    assert_eq!(conversions[0].parameters[0], Ty::Int);
    assert_eq!(conversions[0].result, Ty::String);
}

#[test]
fn fun_interface_constructor_reference_returns_a_checked_sam_delegate() {
    let ir = lower_single_source(
        "fun interface Action { fun run(value: Int): String }\n\
         fun reference(): ((Int) -> String) -> Action = ::Action\n",
        "SamConstructorReference",
    );

    let constructor = ir
        .functions
        .iter()
        .position(|function| function.name.starts_with("$fir_classifier_ref_"))
        .expect("SAM constructor-reference adapter") as u32;
    let delegates = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda {
                impl_fn,
                arity: 1,
                captures,
                sam: Some(target),
                inline_body: None,
            } if captures.len() == 1 => Some((*impl_fn, target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(delegates.len(), 1);
    assert!(delegates[0].1.classifier.matches("Action"));
    assert_eq!(delegates[0].1.method, "run");
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::Lambda {
            impl_fn,
            arity: 1,
            captures,
            sam: None,
            ..
        } if *impl_fn == constructor && captures.is_empty()
    )));
    assert!(ir.exprs.iter().any(|expression| matches!(
        expression,
        IrExpr::InvokeFunction { params, ret, .. }
            if params == &[Ty::Int] && *ret == Ty::String
    )));
}

#[test]
fn consuming_lowering_materializes_source_object_value() {
    let ir = lower_single_source(
        "object Values { val answer: Int = 42 }\n\
         fun read(): Int = Values.answer\n",
        "ObjectValue",
    );

    assert!(ir
        .exprs
        .iter()
        .any(|expression| matches!(expression, IrExpr::SingletonValue { .. })));
}

#[test]
fn local_callable_reference_stays_semantic_through_common_lowering() {
    let ir = lower_single_source(
        "fun reference(offset: Int): (Int) -> Int {\n    fun selected(value: Int): Int = offset + value\n    return ::selected\n}\n",
        "LocalReference",
    );

    let references = semantic_references(&ir);
    let [reference] = references.as_slice() else {
        panic!("one semantic local callable-reference value")
    };
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::Local { .. }
    ));
    assert_eq!(reference.captures.len(), 1);
    assert_common_ir_has_no_jvm_reference_carrier(&ir);
}

#[test]
fn multi_capture_local_callable_reference_stays_semantic_through_common_lowering() {
    let ir = lower_single_source(
        r#"
            fun reference(): () -> String {
                val first = "O"
                val second = "K"
                fun selected(): String = first + second
                return ::selected
            }
        "#,
        "MultiCaptureLocalReference",
    );

    let references = semantic_references(&ir);
    let [reference] = references.as_slice() else {
        panic!("one semantic local callable-reference value")
    };
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::Local { .. }
    ));
    assert_eq!(reference.captures.len(), 2);
    assert_common_ir_has_no_jvm_reference_carrier(&ir);
}

#[test]
fn constructor_reference_stays_semantic_through_common_lowering() {
    let ir = lower_single_source(
        "class Selected(val value: Int)\nfun reference(): (Int) -> Selected = ::Selected\n",
        "ConstructorReference",
    );

    let references = semantic_references(&ir);
    let [reference] = references.as_slice() else {
        panic!("one semantic constructor-reference value")
    };
    assert!(matches!(
        reference.target,
        crate::ir::IrCallableReferenceTarget::Constructor { .. }
    ));
    assert!(reference.captures.is_empty());
    assert_common_ir_has_no_jvm_reference_carrier(&ir);
}
