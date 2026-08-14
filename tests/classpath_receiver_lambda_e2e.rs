//! A lambda passed to a CLASSPATH (separately-compiled) function's RECEIVER function-type parameter
//! (`build(b: Box.() -> Unit)` in a dependency jar) binds its implicit `this` to the receiver, so a bare
//! member call inside resolves against it. krusty decodes the `@ExtensionFunctionType` annotation + the
//! receiver type argument from the callee's `@Metadata` (emitted by real kotlinc). Round-tripped on a JVM.
use super::common;
use krusty::symbol_source::SymbolSource;
fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}
#[test]
fn classpath_receiver_lambda_compiles_and_runs() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) = common::compile_lib(
        "crl",
        "package lib\n\
         class Box { var v: Int = 0; fun set(x: Int) { v = x } }\n\
         fun build(b: Box.() -> Unit): Box { val box = Box(); box.b(); return box }\n",
    ) else {
        return;
    };
    let cp = vec![libout.clone(), sl.clone()];
    // `build { set(42) }` — `set` is a member of the lambda's implicit `this: Box`, from the classpath.
    let main = "import lib.build\n\
        fun box(): String {\n\
        \x20 val r = build { set(42) }\n\
        \x20 return if (r.v == 42) \"OK\" else \"FAIL ${r.v}\"\n\
        }\n";
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath receiver-lambda");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn classpath_member_receiver_lambda_with_defaulted_prefix_compiles_and_runs() {
    let Some(jh) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping: set JAVA_HOME");
        return;
    };
    let sl = common::stdlib_jar();
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    let Some(libout) = common::compile_libs_ref(
        "cmrl",
        &[(
            "Lib.kt",
            "package lib\n\
         class Box { var v: Int = 0; fun set(x: Int) { v = x } }\n\
         class Other { var v: Int = 0; fun touch(x: Int) { v = x } }\n\
         class Num(val raw: Int) : Number() {\n\
           override fun toByte(): Byte = raw.toByte()\n\
           override fun toDouble(): Double = raw.toDouble()\n\
           override fun toFloat(): Float = raw.toFloat()\n\
           override fun toInt(): Int = raw\n\
           override fun toLong(): Long = raw.toLong()\n\
           override fun toShort(): Short = raw.toShort()\n\
           fun mark() {}\n\
         }\n\
         @JvmInline value class Id(val raw: Int)\n\
         suspend fun load(id: Id): String = id.raw.toString()\n\
         class Holder<T>(val item: T) { fun value(): T = item }\n\
         class GenericUi<T>(val seed: T) { fun bind(init: T.() -> Unit): T { seed.init(); return seed } }\n\
         open class BaseUi {\n\
           fun inherited(init: () -> Unit): Box { val box = Box(); init(); return box }\n\
           fun slotChoice(vararg values: String): Box = Box().also { it.v = 99 }\n\
         }\n\
         class Ui : BaseUi() {\n\
           var last: Box? = null\n\
           fun row(label: String? = null, init: Box.() -> Unit): Box {\n\
             val box = Box(); box.init(); last = box; return box\n\
           }\n\
           fun <T> bind(seed: T, init: T.() -> Unit): T { seed.init(); return seed }\n\
           @Suppress(\"UNCHECKED_CAST\")\n\
           fun <T> explicit(seed: Any, init: T.() -> Unit): T { val value = seed as T; value.init(); return value }\n\
           @Suppress(\"UNCHECKED_CAST\")\n\
           fun <T> choose(seed: Any, init: T.() -> Unit): T { val value = seed as T; value.init(); return value }\n\
           fun choose(seed: Any, init: () -> Unit): Box { init(); return Box() }\n\
           fun <T> pair(first: T, second: T, init: T.() -> Unit): T { first.init(); return first }\n\
           fun <T> nullableSeed(seed: T?, init: T.() -> Unit): T? { seed?.init(); return seed }\n\
           suspend fun <T> suspendPair(first: T, second: T, init: T.() -> Unit): T { first.init(); return first }\n\
           fun <T : Any> continuationCollision(first: T, block: T.() -> Unit, continuation: kotlin.coroutines.Continuation<*>): T { first.block(); return first }\n\
           fun <T> continuationCollision(first: T, init: T.() -> Unit): T { first.init(); return first }\n\
           fun <T : Number> pick(seed: T, init: T.() -> Unit): T { seed.init(); return seed }\n\
           fun <T : CharSequence> pick(seed: T, init: T.() -> Unit): T { seed.init(); return seed }\n\
           fun route(value: String, init: Box.() -> Unit) { Box().init() }\n\
           fun route(value: Int, init: (Other) -> Unit) { init(Other()) }\n\
           suspend fun <T> clash(seed: T, init: T.() -> Unit): T { seed.init(); return seed }\n\
           fun <T : Number> clash(seed: T, init: T.() -> Unit, continuation: kotlin.coroutines.Continuation<*>): T { seed.init(); return seed }\n\
           fun <T> holder(seed: T, init: Holder<T>.() -> Unit): Holder<T> {\n\
             val holder = Holder(seed); holder.init(); return holder\n\
           }\n\
           fun mixed(init: Box.() -> Unit): Box { val box = Box(); box.init(); return box }\n\
           fun mixed(init: () -> Unit): Box { val box = Box(); init(); return box }\n\
           fun only(init: Box.() -> Unit): Box { val box = Box(); box.init(); return box }\n\
           fun only(value: Int): Box = Box().also { it.v = value }\n\
           fun inherited(init: Box.() -> Unit): Box { val box = Box(); box.init(); return box }\n\
           fun <T> variadic(vararg seeds: T, init: T.() -> Unit): T {\n\
             val seed = seeds[0]; seed.init(); return seed\n\
           }\n\
           fun receiverVararg(vararg init: Box.() -> Unit): Box {\n\
             val box = Box(); init.forEach { it(box) }; return box\n\
           }\n\
           fun typed(value: String, init: Box.() -> Unit): Box {\n\
             val box = Box(); box.init(); return box\n\
           }\n\
           fun typed(value: Int, init: Other.() -> Unit): Other {\n\
             val other = Other(); other.init(); return other\n\
           }\n\
           fun blocked(init: () -> Unit) { init() }\n\
           fun <T : Number> bounded(init: () -> Unit) { init() }\n\
           fun slotChoice(value: Int = 20): Box = Box().also { it.v = value }\n\
           fun shadow(init: () -> Unit) { init() }\n\
         }\n\
         fun Ui.blocked(init: Box.() -> Unit) { Box().init() }\n\
         fun <T> Ui.bounded(init: Box.() -> Unit) { Box().init() }\n\
         fun Ui.pair(first: Any?, second: Any?, init: Box.() -> Unit) { Box().init() }\n\
         fun Ui.suspendPair(first: Any?, second: Any?, init: Box.() -> Unit) { Box().init() }\n\
         fun requireBox(value: Box) {}\n\
         class Outer { fun shadow(init: Box.() -> Unit) { Box().init() } }\n\
         fun outer(init: Outer.() -> Unit): Outer { val outer = Outer(); outer.init(); return outer }\n\
         fun panel(init: Ui.() -> Unit): Ui { val ui = Ui(); ui.init(); return ui }\n",
        )],
    ) else {
        return;
    };
    let library = krusty::jvm::classpath::Classpath::new(vec![libout.clone()]);
    let load_candidates = library.functions_in_scope("load", &[krusty::types::type_name("lib")]);
    assert!(
        !load_candidates.is_empty(),
        "source-name index must expose the mangled top-level value-class callable"
    );
    assert!(
        load_candidates.iter().any(|candidate| {
            library
                .meta_functions_name(candidate.owner)
                .iter()
                .any(|function| {
                    function.kotlin_name == "load"
                        && function.jvm_name == candidate.name
                        && function.jvm_desc == Some(candidate.descriptor.as_str())
                        && function.is_suspend()
                })
        }),
        "mangled load candidate must retain its exact suspend metadata descriptor"
    );
    let load = &load_candidates[0];
    let physical_params = [
        krusty::types::Ty::Int,
        krusty::types::Ty::obj("kotlin/coroutines/Continuation"),
    ];
    let physical_ret = krusty::types::Ty::obj("java/lang/Object");
    let load_facts = library.metadata_call_facts_name(
        load.owner,
        &load.name,
        &physical_params,
        &physical_ret,
        false,
        &|name| name.matches("lib/Id").then_some(krusty::types::Ty::Int),
    );
    assert!(load_facts.suspend);
    let load_generic = library
        .aligned_generic_sig_name(
            load.owner,
            &load.name,
            &physical_params,
            &physical_ret,
            &|name| name.matches("lib/Id").then_some(krusty::types::Ty::Int),
        )
        .flatten()
        .expect("exact load metadata generic signature");
    assert_eq!(load_generic.params, [krusty::types::Ty::obj("lib/Id")]);
    let ui = library.find("lib/Ui").expect("compiled Ui class");
    let row = krusty::jvm::metadata::class_functions(&ui)
        .iter()
        .find(|function| function.kotlin_name == "row")
        .expect("row metadata");
    assert_eq!(
        row.value_params
            .iter()
            .map(|parameter| (parameter.recv_fun(), parameter.recv_fun_receiver))
            .collect::<Vec<_>>(),
        [
            (false, None),
            (true, Some(krusty::types::type_name("lib/Box")))
        ],
        "member receiver-function metadata"
    );
    let call_sig = row.member_call_sig();
    assert_eq!(
        call_sig.lambda_receivers,
        [None, Some(krusty::types::Ty::obj("lib/Box"))],
        "member call facts must retain the concrete lambda receiver"
    );
    assert_eq!(call_sig.lambda_receiver_params, [false, true]);
    let bind = krusty::jvm::metadata::class_functions(&ui)
        .iter()
        .find(|function| function.kotlin_name == "bind")
        .expect("bind metadata");
    let bind_call_sig = bind.member_call_sig();
    assert_eq!(bind_call_sig.lambda_receivers, [None, None]);
    assert_eq!(bind_call_sig.lambda_receiver_params, [false, true]);
    let platform = krusty::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        krusty::jvm::classpath::Classpath::new(vec![libout.clone()]),
    ));
    let resolved_ui = platform
        .classifier(krusty::types::type_name("lib/Ui"))
        .expect("resolved Ui library type");
    let collision_members = resolved_ui
        .members
        .iter()
        .filter(|member| member.name == "continuationCollision")
        .collect::<Vec<_>>();
    assert_eq!(collision_members.len(), 2);
    let collision_decoy = collision_members
        .iter()
        .copied()
        .find(|member| member.params.len() == 3)
        .expect("ordinary trailing-Continuation overload");
    assert!(!collision_decoy.suspend());
    assert_eq!(collision_decoy.call_sig.param_names[1], "block");
    let collision_target = collision_members
        .iter()
        .copied()
        .find(|member| member.params.len() == 2)
        .expect("ordinary collision target");
    assert!(!collision_target.suspend());
    assert_eq!(collision_target.call_sig.param_names[1], "init");
    assert_eq!(
        collision_target
            .generic_sig
            .as_ref()
            .map(|signature| signature.formal_bounds.clone()),
        Some(vec![Vec::new()]),
        "the exact ordinary overload must retain its unbounded Kotlin type parameter"
    );
    let resolved_generic_ui = platform
        .classifier(krusty::types::type_name("lib/GenericUi"))
        .expect("resolved GenericUi library type");
    let generic_ui_class = library
        .find("lib/GenericUi")
        .expect("compiled GenericUi class");
    let generic_bind_metadata = krusty::jvm::metadata::class_functions(&generic_ui_class)
        .iter()
        .find(|function| function.kotlin_name == "bind")
        .expect("GenericUi.bind metadata");
    assert_eq!(
        generic_bind_metadata
            .generic_sig
            .as_ref()
            .map(|signature| signature
                .formals
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()),
        Some(vec![]),
        "GenericUi.bind metadata: {generic_bind_metadata:#?}"
    );
    let generic_bind_member = resolved_generic_ui
        .members
        .iter()
        .find(|member| member.name == "bind")
        .expect("GenericUi.bind member");
    let generic_bind_signature = generic_bind_member
        .generic_sig
        .as_ref()
        .expect("GenericUi.bind metadata signature");
    assert!(generic_bind_signature.formals.is_empty());
    assert_eq!(generic_bind_member.call_sig.lambda_receiver_params, [true]);
    let holder = krusty::jvm::metadata::class_functions(&ui)
        .iter()
        .find(|function| function.kotlin_name == "holder")
        .expect("holder metadata");
    let holder_call_sig = holder.member_call_sig();
    assert_eq!(
        holder_call_sig.lambda_receivers,
        [None, Some(krusty::types::Ty::obj("lib/Holder"))]
    );
    assert!(matches!(
        holder.generic_sig.as_ref().and_then(|signature| signature.params.get(1)),
        Some(krusty::types::Ty::Fun(function))
            if matches!(function.params.first(), Some(krusty::types::Ty::Obj(name, arguments))
                if name.matches("lib/Holder") && arguments.len() == 1)
    ));
    let receiver_vararg = krusty::jvm::metadata::class_functions(&ui)
        .iter()
        .find(|function| function.kotlin_name == "receiverVararg")
        .expect("receiverVararg metadata");
    let receiver_vararg_sig = receiver_vararg.member_call_sig();
    assert_eq!(receiver_vararg_sig.vararg_index, Some(0));
    assert_eq!(
        receiver_vararg_sig.lambda_receivers,
        [Some(krusty::types::Ty::obj("lib/Box"))]
    );
    assert_eq!(receiver_vararg_sig.lambda_receiver_params, [true]);
    let variadic = krusty::jvm::metadata::class_functions(&ui)
        .iter()
        .find(|function| function.kotlin_name == "variadic")
        .expect("variadic metadata");
    let variadic_member = resolved_ui
        .members
        .iter()
        .find(|member| member.name == "variadic")
        .expect("variadic member");
    assert_eq!(variadic_member.call_sig.vararg_index, Some(0));
    assert!(matches!(
        variadic.generic_sig.as_ref().and_then(|signature| signature.params.get(1)),
        Some(krusty::types::Ty::Fun(function))
            if function.has_receiver
                && matches!(function.params.first(), Some(krusty::types::Ty::TyParam(name, _)) if *name == "T")
    ));
    let facade = library.find("lib/LibKt").expect("compiled file facade");
    let bounded_extension = krusty::jvm::metadata::package_functions(&facade)
        .iter()
        .find(|function| function.kotlin_name == "bounded" && function.is_extension())
        .expect("bounded extension metadata");
    let bounded_extension_sig = bounded_extension.extension_call_sig();
    assert_eq!(
        bounded_extension_sig.lambda_receivers,
        [Some(krusty::types::Ty::obj("lib/Box"))]
    );
    assert_eq!(bounded_extension_sig.lambda_receiver_params, [true]);
    let cp = vec![libout.clone(), sl.clone()];
    for (case, source) in [
        (
            "generic vararg inference",
            "fun probe() { lib.Ui().variadic(lib.Box()) { set(13) } }",
        ),
        (
            "generic spread vararg inference",
            "fun probe() { lib.Ui().variadic(*arrayOf(lib.Box())) { set(13) } }",
        ),
        (
            "receiver-function vararg",
            "fun probe() { lib.Ui().receiverVararg({ set(14) }, { set(15) }) }",
        ),
        (
            "class-owned generic receiver",
            "fun probe() { val owner: lib.GenericUi<lib.Box> = lib.GenericUi(lib.Box()); owner.bind { set(16) } }",
        ),
        (
            "reference overload applicability",
            "fun probe() { lib.Ui().typed(1) { touch(17) } }",
        ),
        (
            "explicit type arguments exclude ordinary sibling",
            "fun probe() { lib.Ui().choose<lib.Box>(lib.Box()) { set(18) } }",
        ),
        (
            "invalid member bound admits extension",
            "import lib.bounded\nfun probe() { lib.Ui().bounded<String> { set(19) } }",
        ),
        (
            "nullable formal specializes receiver",
            "fun probe() { lib.Ui().nullableSeed(lib.Box()) { set(20) } }",
        ),
        (
            "bounded overload identity",
            "fun probe() { lib.Ui().pick(lib.Num(1)) { mark() } }",
        ),
        (
            "ordinary Continuation overload beside suspend",
            "fun probe(continuation: kotlin.coroutines.Continuation<*>) { lib.Ui().clash(lib.Num(1), { mark() }, continuation) }",
        ),
        (
            "top-level suspend value-class parameter",
            "import lib.load\nsuspend fun probe(): String = load(lib.Id(1))",
        ),
    ] {
        let case_diagnostics = common::front_end_diagnostics(source, &cp, Some(&jdk));
        assert!(
            case_diagnostics.is_empty(),
            "{case} diagnostics: {case_diagnostics:?}"
        );
    }
    // The outer top-level receiver lambda already works. Its implicit row call must map the trailing
    // lambda past the omitted label default and retain Box as the inner lambda receiver.
    let main = "import lib.panel\n\
        fun box(): String {\n\
        \x20 val ui = panel { row { set(42) } }\n\
        \x20 val direct = lib.Ui().row { set(7) }\n\
        \x20 val named = lib.Ui().row(init = { set(8) })\n\
        \x20 val generic = lib.Ui().bind(lib.Box()) { set(9) }\n\
        \x20 val explicit = lib.Ui().explicit<lib.Box>(lib.Box()) { set(10) }\n\
        \x20 val only = lib.Ui().only { set(11) }\n\
        \x20 val inherited = lib.Ui().inherited { set(12) }\n\
        \x20 val typed = lib.Ui().typed(1) { touch(17) }\n\
        \x20 val slotChoice = lib.Ui().slotChoice()\n\
        \x20 return if (ui.last!!.v == 42 && direct.v == 7 && named.v == 8 && generic.v == 9 && explicit.v == 10 && only.v == 11 && inherited.v == 12 && typed.v == 17 && slotChoice.v == 20) \"OK\" else \"FAIL\"\n\
        }\n";
    let diagnostics = common::front_end_diagnostics(main, &cp, Some(&jdk));
    assert!(
        diagnostics.is_empty(),
        "classpath member receiver-lambda diagnostics: {diagnostics:?}"
    );
    // Both overloads are partially applicable before the body is typed. A receiver-function sibling
    // must not lend `Box` to the ordinary zero-argument function overload and hide the ambiguity.
    let ambiguous = "fun probe() { lib.Ui().mixed { set(1) } }";
    let ambiguous_diagnostics = common::front_end_diagnostics(ambiguous, &cp, Some(&jdk));
    assert!(
        ambiguous_diagnostics
            .iter()
            .any(|message| message.contains("unresolved function 'set'")),
        "mixed receiver/non-receiver overloads must defer lambda typing: {ambiguous_diagnostics:?}"
    );
    let blocked_by_member = "import lib.blocked\nfun probe() { lib.Ui().blocked { set(1) } }";
    let blocked_by_member_diagnostics =
        common::front_end_diagnostics(blocked_by_member, &cp, Some(&jdk));
    assert!(
        blocked_by_member_diagnostics
            .iter()
            .any(|message| message.contains("unresolved function 'set'")),
        "an ordinary member must block lower-priority extension pretyping: {blocked_by_member_diagnostics:?}"
    );
    let ordinary_function_lambda = "fun probe() { lib.Ui().route(1) { touch(1) } }";
    let ordinary_function_lambda_diagnostics =
        common::front_end_diagnostics(ordinary_function_lambda, &cp, Some(&jdk));
    assert!(
        ordinary_function_lambda_diagnostics
            .iter()
            .any(|message| message.contains("'touch'")),
        "an ordinary function parameter must not inherit a sibling overload's lambda receiver: {ordinary_function_lambda_diagnostics:?}"
    );
    let joined_member_receiver =
        "import lib.pair\nfun probe() { lib.Ui().pair(lib.Box(), lib.Other()) { set(1) } }";
    let joined_member_receiver_diagnostics =
        common::front_end_diagnostics(joined_member_receiver, &cp, Some(&jdk));
    assert!(
        joined_member_receiver_diagnostics
            .iter()
            .any(|message| message.contains("unresolved function 'set'")),
        "an applicable member with a joined generic receiver must block extension pretyping: {joined_member_receiver_diagnostics:?}"
    );
    for (case, source) in [
        (
            "nullable value",
            "import lib.pair\nimport lib.requireBox\nfun probe(value: lib.Box?) { lib.Ui().pair(value, lib.Box()) { requireBox(this) } }",
        ),
        (
            "null literal",
            "import lib.pair\nimport lib.requireBox\nfun probe() { lib.Ui().pair(null, lib.Box()) { requireBox(this) } }",
        ),
        (
            "suspend member",
            "import lib.requireBox\nimport lib.suspendPair\nsuspend fun probe(value: lib.Box?) { lib.Ui().suspendPair(value, lib.Box()) { requireBox(this) } }",
        ),
    ] {
        let nullable_join_diagnostics = common::front_end_diagnostics(source, &cp, Some(&jdk));
        assert!(
            nullable_join_diagnostics
                .iter()
                .any(|message| message.contains("actual type is 'lib.Box?'")
                    && message.contains("'lib.Box' was expected")),
            "{case} must keep the nullable member receiver and block extension pretyping: {nullable_join_diagnostics:?}"
        );
    }
    let blocked_by_inner_receiver =
        "import lib.outer\nimport lib.panel\nfun probe() { outer { panel { shadow { set(1) } } } }";
    let blocked_by_inner_receiver_diagnostics =
        common::front_end_diagnostics(blocked_by_inner_receiver, &cp, Some(&jdk));
    assert!(
        blocked_by_inner_receiver_diagnostics
            .iter()
            .any(|message| message.contains("unresolved function 'set'")),
        "a nearer ordinary member must block outer implicit receiver pretyping: {blocked_by_inner_receiver_diagnostics:?}"
    );
    let classes = common::compile_in_process(main, "Main", &cp, Some(&jdk))
        .expect("krusty failed to compile a classpath member receiver-lambda");
    match common::run_box(&classes, "MainKt", &[libout, sl]) {
        Some(o) => assert_eq!(o.trim(), "OK", "box() = {o:?}"),
        None => eprintln!("skipping: box runner unavailable"),
    }
}

#[test]
fn inapplicable_bounded_member_does_not_block_extension_receiver_lambda() {
    let Some(libout) = common::compile_lib_ref(
        "receiver_lambda_conflicting_bound",
        "package lib\n\
         class Ui {\n\
           fun <T : Number> choose(first: T, second: T, init: T.() -> Unit) {}\n\
         }\n\
         fun Ui.choose(first: Any, second: Any, init: String.() -> Unit) { \"\".init() }\n",
    ) else {
        return;
    };
    let classpath = [libout, common::stdlib_jar()];
    let source = "import lib.Ui\n\
        import lib.choose\n\
        fun probe() { Ui().choose(1, \"x\") { length } }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the extension-selected call: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty did not match kotlinc: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

#[test]
fn postponed_callable_reference_does_not_reject_receiver_lambda_candidate() {
    let classpath = [common::stdlib_jar()];
    let source = "fun marker() {}\n\
        fun withRef(ref: kotlin.reflect.KFunction<*>, init: String.() -> Unit) { \"\".init() }\n\
        fun probe() { withRef(::marker) { length } }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected the callable-reference call: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty did not match kotlinc: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}

#[test]
fn stdlib_join_to_string_trailing_lambda_remains_applicable() {
    let classpath = [common::stdlib_jar()];
    let source = "fun probe(xs: List<String>): String {\n\
        \x20 val implicitDefaults = xs.joinToString { it.length.toString() }\n\
        \x20 return implicitDefaults + xs.joinToString(\"-\") { it.uppercase() }\n\
        }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", source)], &classpath);
    assert_eq!(
        result.reference_code, 0,
        "kotlinc rejected joinToString trailing lambdas: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 0,
        "krusty did not match kotlinc: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
