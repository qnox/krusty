use super::*;

#[test]
fn aliased_type_annotation_is_stored_as_its_classifier_identity() {
    let mut diagnostics = DiagSink::new();
    let declaration = parse_file(
        "package kotlin.internal\nannotation class Exact",
        &mut diagnostics,
    );
    let use_site = parse_file(
        "import kotlin.internal.Exact as Constraint\n\
             fun <T> same(first: @Constraint T, second: T) {}",
        &mut diagnostics,
    );
    let symbols = collect_signatures(&[declaration, use_site], &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    assert_eq!(symbols.funs["same"][0].exact_params, [true, false]);
}

#[test]
fn nested_type_parameter_annotation_uses_lexical_classifier_identity() {
    let source = r#"
package sample

annotation class Mark

class Host {
    annotation class Mark
    fun <@Mark T> keep(value: T): T = value
}
"#;
    let mut diagnostics = DiagSink::new();
    let (file, symbols, _) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    let host = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) if class.name == "Host" => Some(class),
            _ => None,
        })
        .expect("Host class");
    assert!(file.decls.iter().any(|&declaration| {
        matches!(
            file.decl(declaration),
            Decl::Class(class)
                if class.name == "Host.Mark" && class.kind == ClassKind::Annotation
        )
    }));
    let method = host
        .methods
        .iter()
        .find(|method| method.name == "keep")
        .expect("keep method");
    let annotation = file.declaration_type_parameter_annotations[&method.signature_span.lo][0]
        .annotations[0]
        .clone();

    assert_no_diags(&diagnostics);
    assert!(
        symbols
            .resolved_annotation(0, &annotation)
            .is_some_and(|identity| identity.matches("sample/Host$Mark")),
        "nested annotation should bind to Host.Mark, got {:?}",
        symbols.resolved_annotation(0, &annotation)
    );
}

#[test]
fn nested_member_annotation_uses_lexical_classifier_identity() {
    let source = r#"
package sample

annotation class Mark

class Host {
    annotation class Mark
    @Mark fun keep(): String = "OK"
}
"#;
    let mut diagnostics = DiagSink::new();
    let (file, symbols, info) =
        crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let symbols = symbols.expect("production frontend must retain finalized symbols");
    assert!(info.is_some(), "production frontend must check the source");
    let host = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class) if class.name == "Host" => Some(class),
            _ => None,
        })
        .expect("Host class");
    let annotation = host
        .methods
        .iter()
        .find(|method| method.name == "keep")
        .expect("keep method")
        .annotations[0]
        .clone();

    assert!(
        symbols
            .resolved_annotation(0, &annotation)
            .is_some_and(|identity| identity.matches("sample/Host$Mark")),
        "nested annotation should bind to Host.Mark, got {:?}",
        symbols.resolved_annotation(0, &annotation)
    );
    assert_no_diags(&diagnostics);
}

#[test]
fn unresolved_declaration_type_parameter_annotation_is_an_error() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file("class Box<@DefinitelyAbsentAnnotation T>", &mut diagnostics);

    let symbols = collect_signatures(&[file], &mut diagnostics);

    assert!(symbols.resolved_annotations.is_empty());
    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        ["unresolved reference 'DefinitelyAbsentAnnotation'."]
    );
}

#[test]
fn declaration_type_parameter_annotation_arguments_are_checked() {
    let source = r#"
annotation class Marker(val value: String)
class Box<@Marker(1) T>
class Target<T>
typealias Alias<@Marker(2) T> = Target<T>
"#;
    let (_, _, diagnostics) = retained_standalone_analysis(source);

    assert_eq!(
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.as_str())
            .collect::<Vec<_>>(),
        [
            "argument type mismatch: actual type is 'Int', but 'String' was expected.",
            "argument type mismatch: actual type is 'Int', but 'String' was expected.",
        ],
        "class and typealias annotations must use the normal argument checker",
    );
}

#[test]
fn every_nested_declaration_type_parameter_annotation_gets_its_lexical_identity() {
    let mut diagnostics = DiagSink::new();
    let file = parse_file(
        r#"
package sample

annotation class Mark

class Host {
    annotation class Mark

    fun outer() {
        fun <@Mark T> local(value: T): T = value
    }
}

enum class Choice {
    ENTRY {
        fun <@Mark T> entry(value: T): T = value
    };

    annotation class Mark
}

interface Contract {
    annotation class Mark
    fun <@Mark T> member(value: T): T
}

open class Base {
    annotation class Mark
}

class Derived : Base() {
    fun <@Mark T> inherited(value: T): T = value
}

enum class EntryChoice {
    ENTRY {
        annotation class Mark
        annotation class EntryOnly
        fun <@Mark T> entryLocal(value: T): T = value
        fun localOwner() {
            class Local<@EntryOnly T> {
                annotation class Mark
                class Nested<@Mark U, @EntryOnly V>
            }
        }
    };

    annotation class Mark
}
"#,
        &mut diagnostics,
    );
    let class = |name: &str| {
        file.decls
            .iter()
            .find_map(|&declaration| match file.decl(declaration) {
                Decl::Class(class) if class.name == name => Some(class),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing class {name}"))
    };
    let host = class("Host");
    let outer = host
        .methods
        .iter()
        .find(|method| method.name == "outer")
        .expect("outer method");
    let FunBody::Block(outer_body) = outer.body else {
        panic!("outer block body");
    };
    let Expr::Block { stmts, .. } = file.expr(outer_body) else {
        panic!("outer block expression");
    };
    let local = stmts
        .iter()
        .find_map(|&statement| match file.stmt(statement) {
            Stmt::LocalFun(function) if function.name == "local" => Some(function),
            _ => None,
        })
        .expect("local function");
    let choice = class("Choice");
    let entry = choice.enum_entries[0]
        .methods
        .iter()
        .find(|method| method.name == "entry")
        .expect("enum-entry function");
    let contract = class("Contract");
    let member = contract
        .methods
        .iter()
        .find(|method| method.name == "member")
        .expect("interface member");
    let derived = class("Derived");
    let inherited = derived
        .methods
        .iter()
        .find(|method| method.name == "inherited")
        .expect("inherited nested annotation method");
    let entry_choice = class("EntryChoice");
    assert!(file.decls.iter().any(|&declaration| {
        matches!(
            file.decl(declaration),
            Decl::Class(class) if class.name == "EntryChoice.ENTRY.EntryOnly"
        )
    }));
    assert!(file.decls.iter().any(|&declaration| {
        matches!(
            file.decl(declaration),
            Decl::Class(class)
                if class.name.ends_with("EntryChoice.ENTRY.localOwner.Local.Mark")
        )
    }));
    let entry_local = entry_choice.enum_entries[0]
        .methods
        .iter()
        .find(|method| method.name == "entryLocal")
        .expect("entry-local nested annotation method");
    let entry_local_class = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class)
                if class.name.starts_with("EntryChoice.ENTRY.localOwner.")
                    && class.name.ends_with("Local") =>
            {
                Some(class)
            }
            _ => None,
        })
        .expect("enum-entry local class");
    let entry_local_nested_class = file
        .decls
        .iter()
        .find_map(|&declaration| match file.decl(declaration) {
            Decl::Class(class)
                if class.name.starts_with("EntryChoice.ENTRY.localOwner.")
                    && class.name.ends_with("Local.Nested") =>
            {
                Some(class)
            }
            _ => None,
        })
        .expect("class nested inside an enum-entry local class");
    let annotations = [
        (
            file.declaration_type_parameter_annotations[&local.signature_span.lo][0].annotations[0]
                .clone(),
            "sample/Host$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&entry.signature_span.lo][0].annotations[0]
                .clone(),
            "sample/Choice$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&member.signature_span.lo][0].annotations
                [0]
            .clone(),
            "sample/Contract$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&inherited.signature_span.lo][0]
                .annotations[0]
                .clone(),
            "sample/Base$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&entry_local.signature_span.lo][0]
                .annotations[0]
                .clone(),
            "sample/EntryChoice$ENTRY$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&entry_local_class.span.lo][0].annotations
                [0]
            .clone(),
            "sample/EntryChoice$ENTRY$EntryOnly",
        ),
        (
            file.declaration_type_parameter_annotations[&entry_local_nested_class.span.lo][0]
                .annotations[0]
                .clone(),
            "sample/EntryChoice$ENTRY$localOwner$Local$Mark",
        ),
        (
            file.declaration_type_parameter_annotations[&entry_local_nested_class.span.lo][1]
                .annotations[0]
                .clone(),
            "sample/EntryChoice$ENTRY$EntryOnly",
        ),
    ];
    // This focused probe deliberately retains the private signature-collection path:
    // production stable-header analysis still needs a structural lexical-owner edge from an
    // enum-entry body into a hoisted local class before these two EntryOnly occurrences can be
    // migrated without losing their semantic identity.
    let symbols = collect_signatures(&[file], &mut diagnostics);

    assert_no_diags(&diagnostics);
    for (annotation, expected) in annotations {
        assert!(
            symbols
                .resolved_annotation(0, &annotation)
                .is_some_and(|identity| identity.matches(expected)),
            "annotation should bind to {expected}, got {:?}",
            symbols.resolved_annotation(0, &annotation)
        );
    }
}
