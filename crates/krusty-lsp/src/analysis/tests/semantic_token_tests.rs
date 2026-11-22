//! Semantic-token classification of analysed sources.

use super::*;

#[test]
fn semantic_tokens_match_official_kotlin_symbol_classification() {
    let source = concat!(
        "data class User(val name: String)\n",
        "fun greet(user: User): String = user.name"
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

    assert_eq!(
        index.encode(None),
        vec![
            0, 11, 4, 4, 1, // data-class declaration: struct + declaration
            0, 9, 4, 7,
            5, // val constructor property declaration: parameter + declaration + readonly
            0, 6, 6, 1, 512, // kotlin.String: class + defaultLibrary
            1, 4, 5, 12, 9, // top-level function: function + declaration + static
            0, 6, 4, 7, 5, // value parameter: parameter + declaration + readonly
            0, 6, 4, 4, 0, // data-class type reference: struct
            0, 7, 6, 1, 512, // kotlin.String return: class + defaultLibrary
            0, 9, 4, 7, 4, // parameter reference: parameter + readonly
            0, 5, 4, 9, 4, // immutable property reference: property + readonly
        ]
    );
    assert_eq!(std::mem::size_of::<SemanticTokenEntry>(), 16);
}

#[test]
fn semantic_token_range_encoding_rebases_the_first_token() {
    let source = "fun first() = 1\nfun second() = first()";
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

    assert_eq!(
        index.encode(Some(SemanticTokenRange {
            start_line: 1,
            start_character: 0,
            end_line: 2,
            end_character: 0,
        })),
        vec![
            1, 4, 6, 12, 9, // second declaration
            0, 11, 5, 12, 8, // first reference
        ]
    );
}

#[test]
fn semantic_tokens_match_official_constructor_and_enum_modifiers() {
    let source = concat!(
        "enum class Shade { RED }\n",
        "class Holder(var mutable: Int, val fixed: Int)\n",
        "fun paint(holder: Holder): Shade {\n",
        "holder.mutable = holder.fixed\n",
        "return Shade.RED\n",
        "}\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(0, 19, 3, 10, 5))); // enum entry declaration: declaration + readonly
    assert!(tokens.contains(&(1, 17, 7, 7, 5))); // `var` property parameter: parameter + readonly
    assert!(tokens.contains(&(3, 7, 7, 9, 128))); // mutable property assignment
    assert!(tokens.contains(&(4, 13, 3, 10, 4))); // enum entry reference: readonly
}

#[test]
fn semantic_tokens_skip_control_flow_labels_and_mark_compound_assignment() {
    let source = concat!(
        "fun sumPositive(values: IntArray): Int {\n",
        "var result = 0\n",
        "outer@ for (value in values) {\n",
        "if (value < 0) continue@outer\n",
        "if (value == 0) break@outer\n",
        "result += value\n",
        "}\n",
        "return result\n",
        "}\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(
        !tokens
            .iter()
            .any(|&(line, start, length, _, _)| (line, start, length) == (3, 24, 5)),
        "control-flow labels are not semantic symbols: {tokens:?}"
    );
    assert!(
        !tokens
            .iter()
            .any(|&(line, start, length, _, _)| (line, start, length) == (4, 22, 5)),
        "control-flow labels are not semantic symbols: {tokens:?}"
    );
    assert!(
        tokens.contains(&(5, 7, 2, 21, 512)),
        "builtin compound assignment is an operator: {tokens:?}"
    );

    // Standalone analysis has no stdlib, so the source declares the `let` that shapes `it`.
    let return_source = "fun pick(n: Int): Int = n.let { if (it > 0) return@let it; return 0 }\n\
                         inline fun <T, R> T.let(block: (T) -> R): R = block(this)";
    let return_analysis = analyze_standalone_source_set(&[return_source]);
    let return_index = SemanticTokenIndex::from_file_analysis(
        return_source,
        &return_analysis.files[0],
        &return_analysis.symbols,
    );
    let return_tokens = decoded_tokens(&return_index);
    let label_start = return_source.find("@let").unwrap() as u32;
    let first_it = return_source.find("(it").unwrap() as u32 + 1;
    let returned_it = return_source.find("@let it").unwrap() as u32 + 5;
    assert!(
        return_tokens.contains(&(0, label_start, 4, 12, 0)),
        "labeled returns target a function at the full `@label` span: {return_tokens:?}"
    );
    assert!(
        return_tokens.contains(&(0, first_it, 2, 7, 4))
            && return_tokens.contains(&(0, returned_it, 2, 7, 4)),
        "implicit lambda it is a readonly parameter: {return_tokens:?}"
    );

    let receiver_source = concat!(
        "interface Base { fun value(): Int = 1 }\n",
        "class Outer : Base {\n",
        "override fun value(): Int = super<Base>@Outer.value()\n",
        "fun self(): Outer = noreturn@ this\n",
        "}\n",
    );
    let receiver_analysis = analyze_standalone_source_set(&[receiver_source]);
    let receiver_index = SemanticTokenIndex::from_file_analysis(
        receiver_source,
        &receiver_analysis.files[0],
        &receiver_analysis.symbols,
    );
    let receiver_tokens = decoded_tokens(&receiver_index);
    let receiver_line = receiver_source.lines().nth(2).unwrap();
    let super_start = receiver_line.find("super").unwrap() as u32;
    let base_start = receiver_line.find("<Base>").unwrap() as u32 + 1;
    let receiver_label = receiver_line.find("@Outer").unwrap() as u32;
    let this_start = receiver_source
        .lines()
        .nth(3)
        .unwrap()
        .find("this")
        .unwrap() as u32;
    assert!(
        receiver_tokens.contains(&(2, super_start, 5, 3, 32))
            && receiver_tokens.contains(&(2, base_start, 4, 3, 32))
            && receiver_tokens.contains(&(2, receiver_label, 6, 1, 0))
            && receiver_tokens.contains(&(3, this_start, 4, 1, 0)),
        "receiver labels use exact class/interface spans: {receiver_tokens:?}"
    );
}

#[test]
fn semantic_tokens_mark_user_compound_assignment_as_user_operator() {
    let source = concat!(
        "class Accumulator {\n",
        "operator fun plusAssign(value: Int) {}\n",
        "}\n",
        "operator fun Accumulator.minusAssign(value: Int) {}\n",
        "fun use(acc: Accumulator) {\n",
        "acc += 1\n",
        "acc -= 1\n",
        "}\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(
        tokens.contains(&(5, 4, 2, 21, 0)),
        "member plusAssign is a user operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(6, 4, 2, 21, 8)),
        "extension minusAssign is a static user operator: {tokens:?}"
    );
}

#[test]
fn semantic_tokens_match_official_unary_inc_and_constructor_sites() {
    let source = concat!(
        "class Counter {\n",
        "operator fun unaryMinus(): Counter = this\n",
        "operator fun inc(): Counter = this\n",
        "}\n",
        "class Marker\n",
        "operator fun Marker.not(): Marker = this\n",
        "fun use(input: Int, counter: Counter) {\n",
        "var current = counter\n",
        "val builtin = -input\n",
        "val user = -current\n",
        "current++\n",
        "val previous = current++\n",
        "val extension = !Marker()\n",
        "}\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(
        tokens.contains(&(1, 37, 4, 1, 0)),
        "member this: {tokens:?}"
    );
    assert!(
        tokens.contains(&(8, 14, 1, 21, 512)),
        "builtin unary operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(9, 11, 1, 21, 0)),
        "member unary operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(10, 7, 2, 21, 0)),
        "statement increment operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(11, 22, 2, 21, 0)),
        "expression increment operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(12, 16, 1, 21, 8)),
        "extension unary operator: {tokens:?}"
    );
    assert!(
        tokens.contains(&(12, 17, 6, 13, 0)),
        "constructor invocation: {tokens:?}"
    );
}

#[test]
fn semantic_tokens_use_inc_target_for_member_and_index_storage() {
    let source = concat!(
        "class StorageCounter { operator fun inc(): StorageCounter = this }\n",
        "class StorageHolder(var value: StorageCounter)\n",
        "class StorageCounters(var value: StorageCounter) {\n",
        "operator fun get(index: Int): StorageCounter = value\n",
        "operator fun set(index: Int, value: StorageCounter) { this.value = value }\n",
        "}\n",
        "fun useStorage(holder: StorageHolder, counters: StorageCounters) {\n",
        "holder.value++\n",
        "counters[0]++\n",
        "}\n",
    );
    let platform = krusty::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        toolchain::stdlib_classpath(),
    ));
    let analysis = analyze_source_set(&[source], PlatformProvider::jvm(platform));
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);
    let types = analysis.files[0].types.as_ref().expect("checked types");
    let inc_calls = analysis.files[0]
        .file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let ast::Expr::Call { callee, args } = expression else {
                return None;
            };
            matches!(
                analysis.files[0].file.expr(*callee),
                ast::Expr::Member { name, .. } if name == "inc" && args.is_empty()
            )
            .then_some(ast::ExprId(index as u32))
        })
        .collect::<Vec<_>>();

    assert!(
        tokens.contains(&(7, 12, 2, 21, 0)),
        "member-storage increment uses StorageCounter.inc; calls={:?}: {tokens:?}",
        inc_calls
            .iter()
            .map(|&call| (
                call,
                types.resolved_call_is_member(call),
                types.resolved_call_is_extension(call),
                types.resolved_call_owner(call)
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        !tokens
            .iter()
            .any(|&(line, start, length, _, _)| (line, start, length) == (8, 11, 2)),
        "official Kotlin LSP omits the index-storage increment token: {tokens:?}"
    );
}

#[test]
fn semantic_tokens_match_official_advanced_symbol_classification() {
    let source = concat!(
        "package tokenparity\n",
        "@Deprecated(\"old\") data class Record(val value: Int)\n",
        "interface Named { fun name(): String }\n",
        "object Registry { var current: Record? = null }\n",
        "enum class State { READY }\n",
        "typealias Alias = Record\n",
        "operator fun Record.plus(other: Record): Record = this\n",
        "fun use(input: Alias): Int {\n",
        "Registry.current = input\n",
        "return State.READY.ordinal + input.value\n",
        "}\n",
    );
    let platform = krusty::jvm::jvm_libraries::JvmLibraries::new(std::rc::Rc::new(
        toolchain::stdlib_classpath(),
    ));
    let analysis = analyze_source_set(&[source], PlatformProvider::jvm(platform));
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(1, 1, 10, 13, 0))); // annotation application: method
    assert!(tokens.contains(&(2, 10, 5, 3, 33))); // interface declaration: abstract
    assert!(tokens.contains(&(2, 22, 4, 13, 33))); // bodyless interface method: abstract
    assert!(tokens.contains(&(5, 10, 5, 4, 17))); // deprecated alias declaration
    assert!(tokens.contains(&(5, 18, 6, 4, 16))); // deprecated alias target
    assert!(tokens.contains(&(6, 13, 6, 4, 16))); // extension receiver type
    assert!(tokens.contains(&(7, 15, 5, 4, 16))); // alias use inherits target kind/modifiers
    assert!(!tokens
        .iter()
        .any(|&(line, start, _, _, _)| { line == 6 && start == 50 })); // `this` is not a semantic name token
    assert!(tokens.contains(&(9, 19, 7, 9, 516)), "tokens: {tokens:?}"); // inherited enum property
    assert!(tokens.contains(&(9, 27, 1, 21, 512))); // builtin operator
}

#[test]
fn semantic_tokens_follow_type_alias_chains() {
    let source = concat!(
        "@Deprecated(\"old\") data class Record(val value: Int)\n",
        "typealias Alias = Record\n",
        "typealias Alias2 = Alias\n",
        "fun use(input: Alias2): Int = input.value\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(2, 10, 6, 4, 17)));
    assert!(tokens.contains(&(2, 19, 5, 4, 16)));
    assert!(tokens.contains(&(3, 15, 6, 4, 16)));
}

#[test]
fn semantic_token_range_includes_a_token_intersecting_its_start() {
    let source = "fun highlighted() = 1";
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);

    assert_eq!(
        index.encode(Some(SemanticTokenRange {
            start_line: 0,
            start_character: 8,
            end_line: 0,
            end_character: 10,
        })),
        vec![0, 4, 11, 12, 9]
    );
}

#[test]
fn semantic_tokens_respect_lexical_shadowing_between_functions() {
    let source = concat!(
        "fun withParameter(item: Int) = item\n",
        "fun withLocal(): Int { val item = 1; return item }\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(1, 44, 4, 8, 4)));
}

#[test]
fn semantic_tokens_cover_alias_operator_deprecation_and_member_resolution() {
    let source = concat!(
        "typealias Label = String\n",
        "@Deprecated class Old\n",
        "class Box {\n",
        "  operator fun get(i: Int): Int = i\n",
        "  fun target(): Int = 1\n",
        "  fun caller(): Int = get(0) + target()\n",
        "}\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(0, 10, 5, 1, 513))); // alias: expanded class + declaration + stdlib
    assert!(tokens.contains(&(1, 18, 3, 1, 17))); // deprecated class declaration
    assert!(tokens.contains(&(3, 15, 3, 21, 1))); // operator declaration
    assert!(tokens.contains(&(5, 22, 3, 21, 0))); // implicit-receiver operator call
    assert!(tokens.contains(&(5, 31, 6, 13, 0))); // implicit-receiver method call
}

#[test]
fn semantic_tokens_resolve_the_terminal_import_symbol() {
    let source = "import kotlin.String\nfun echo(value: String) = value";
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(0, 7, 6, 0, 0))); // kotlin namespace
    assert!(tokens.contains(&(0, 14, 6, 1, 512))); // imported class, not a namespace
}

#[test]
fn semantic_tokens_resolve_qualified_members_and_deprecated_references() {
    let source = concat!(
        "@Deprecated class Old\n",
        "enum class Color { RED }\n",
        "class A(val value: Int)\n",
        "class B(var value: Int)\n",
        "class Box { operator fun get(i: Int): Int = i; fun target(): Int = 1 }\n",
        "typealias Callback = (Int) -> Int\n",
        "fun inspect(a: A, b: B, box: Box): Int = a.value + b.value + box.get(0)\n",
        "fun reference() = Box::target\n",
        "fun color(): Color = Color.RED\n",
        "fun old(): Old = Old()\n",
    );
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let tokens = decoded_tokens(&index);
    let lines: Vec<_> = source.lines().collect();

    assert!(tokens.contains(&(5, 10, 8, 3, 513))); // function alias: stdlib interface declaration
    assert!(tokens.contains(&(6, lines[6].find("value").unwrap() as u32, 5, 9, 4,))); // A.value: readonly
    assert!(tokens.contains(&(6, lines[6].find("b.value").unwrap() as u32 + 2, 5, 9, 128,))); // B.value: mutable
    assert!(tokens.contains(&(6, lines[6].find("get").unwrap() as u32, 3, 21, 0,))); // qualified operator call
    assert!(tokens.contains(&(7, lines[7].find("target").unwrap() as u32, 6, 13, 0)));
    assert!(tokens.contains(&(8, lines[8].find("RED").unwrap() as u32, 3, 10, 4)));
    assert!(tokens.contains(&(9, lines[9].find("Old").unwrap() as u32, 3, 1, 16)));
    assert!(tokens.contains(&(9, lines[9].rfind("Old").unwrap() as u32, 3, 13, 0)));
}

#[test]
fn semantic_tokens_preserve_source_set_metadata_across_files() {
    let declaration = concat!(
        "@Deprecated data class Model(val value: Int)\n",
        "class Box { operator fun get(i: Int): Int = i }\n",
    );
    let usage = "fun use(model: Model, box: Box): Model { box.get(0); return model }";
    let sources = [declaration, usage];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        usage,
        &analysis.files[1],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(0, 15, 5, 4, 16))); // cross-file data/deprecated parameter type
    assert!(tokens.contains(&(0, 33, 5, 4, 16))); // cross-file data/deprecated return type
    assert!(tokens.contains(&(0, 45, 3, 21, 0))); // cross-file operator member
}

#[test]
fn semantic_tokens_keep_same_named_source_classifiers_package_qualified() {
    // Editor classification must follow the same internal classifier identity as the compiler.
    // A global simple-name table would let the later class declaration overwrite the imported
    // enum's kind and entries in the usage file.
    let first = "package first\nenum class Marker { ONE }";
    let second = "package second\nclass Marker { fun other(): Int = 1 }";
    let usage = "package use\nimport first.Marker\nfun pick(): Marker = Marker.ONE";
    let sources = [first, second, usage];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        usage,
        &analysis.files[2],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);
    let lines = usage.lines().collect::<Vec<_>>();

    assert!(tokens.contains(&(1, lines[1].find("Marker").unwrap() as u32, 6, 2, 0,)));
    assert!(tokens.contains(&(2, lines[2].rfind("ONE").unwrap() as u32, 3, 10, 4,)));
}

#[test]
fn semantic_tokens_keep_type_alias_metadata_package_qualified() {
    let deprecated = "package old\n\
                      @Deprecated(\"old\") data class Record(val value: Int)\n\
                      typealias Alias = Record\n\
                      fun use(value: Alias): Alias = value\n";
    let plain = "package plain\n\
                 data class Record(val value: Int)\n\
                 typealias Alias = Record\n";
    let sources = [deprecated, plain];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        deprecated,
        &analysis.files[0],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(3, 15, 5, 4, 16)), "{tokens:?}");
    assert!(tokens.contains(&(3, 23, 5, 4, 16)), "{tokens:?}");
}

#[test]
fn semantic_tokens_do_not_resolve_alias_cycles_through_bare_name_collisions() {
    let cycle = "package cycle\n\
                 typealias A = B\n\
                 typealias B = A\n\
                 fun use(value: A): A = value\n";
    let unrelated = "package other\n@Deprecated(\"old\") data class A(val value: Int)\n";
    let sources = [cycle, unrelated];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        cycle,
        &analysis.files[0],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    for expected in [
        (1, 10, 1, 1, 1),
        (1, 14, 1, 1, 0),
        (2, 10, 1, 1, 1),
        (2, 14, 1, 1, 0),
        (3, 15, 1, 1, 0),
        (3, 19, 1, 1, 0),
    ] {
        assert!(tokens.contains(&expected), "{tokens:?}");
    }
}

#[test]
fn semantic_tokens_do_not_escape_alias_cycles_through_default_imports() {
    let cycle = "package cycle\n\
                 typealias Trap = Other\n\
                 typealias Other = Trap\n\
                 fun use(value: Other): Other = value\n";
    let default_import = "package kotlin\n@Deprecated(\"old\") data class Trap(val value: Int)\n";
    let sources = [cycle, default_import];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        cycle,
        &analysis.files[0],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    // The source aliases are the selected declarations. A same-named classifier in a lower
    // default-import level must not lend them the `defaultLibrary` modifier while the cycle is
    // unresolved; doing so would recreate the spelling-based fallback this regression forbids.
    for expected in [
        (1, 10, 4, 1, 1),
        (1, 17, 5, 1, 0),
        (2, 10, 5, 1, 1),
        (2, 18, 4, 1, 0),
        (3, 15, 5, 1, 0),
        (3, 23, 5, 1, 0),
    ] {
        assert!(tokens.contains(&expected), "{tokens:?}");
    }
}

#[test]
fn semantic_tokens_follow_imported_alias_chains() {
    let model = "package model\n\
                 @Deprecated(\"old\") data class Record(val value: Int)\n\
                 typealias Alias = Record\n";
    let bridge = "package bridge\n\
                  import model.Alias\n\
                  typealias Alias2 = Alias\n";
    let consumer = "package consumer\n\
                    import bridge.Alias2\n\
                    fun use(value: Alias2): Alias2 = value\n";
    let sources = [model, bridge, consumer];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        consumer,
        &analysis.files[2],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    assert!(tokens.contains(&(2, 15, 6, 4, 16)), "{tokens:?}");
    assert!(tokens.contains(&(2, 24, 6, 4, 16)), "{tokens:?}");
}

#[test]
fn semantic_tokens_preserve_ambiguous_alias_targets() {
    let deprecated = "package left\n@Deprecated(\"old\") data class Target(val value: Int)\n";
    let plain = "package right\ndata class Target(val value: Int)\n";
    let default_import = "package kotlin\n@Deprecated(\"old\") annotation class Target\n";
    let consumer = "package consumer\n\
                    import left.*\n\
                    import right.*\n\
                    typealias Alias = Target\n\
                    fun use(value: Alias): Alias = value\n";
    let sources = [deprecated, plain, default_import, consumer];
    let analysis = analyze_standalone_source_set(&sources);
    let highlight_symbols = HighlightSymbols::from_source_set(&analysis.files, &analysis.symbols);
    let index = SemanticTokenIndex::from_source_set_file_analysis(
        consumer,
        &analysis.files[3],
        &analysis.symbols,
        &highlight_symbols,
    );
    let tokens = decoded_tokens(&index);

    for expected in [
        (3, 10, 5, 1, 513),
        (3, 18, 6, 1, 512),
        (4, 15, 5, 1, 512),
        (4, 23, 5, 1, 512),
    ] {
        assert!(tokens.contains(&expected), "{tokens:?}");
    }
}

#[test]
fn semantic_token_worker_snapshot_uses_compact_array_entries() {
    let source = "fun answer(): Int = 42";
    let analysis = analyze_standalone_source_set(&[source]);
    let index =
        SemanticTokenIndex::from_file_analysis(source, &analysis.files[0], &analysis.symbols);
    let json = serde_json::to_value(&index).unwrap();

    assert!(json["entries"][0].is_array());
    assert_eq!(json["entries"][0].as_array().unwrap().len(), 4);
}
