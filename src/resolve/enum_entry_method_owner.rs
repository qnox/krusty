use super::*;
use crate::lexer::lex;
use crate::parser::parse;

#[test]
fn enum_entry_method_keeps_the_nested_owner() {
    let mut diagnostics = DiagSink::new();
    let source = "package sample\n\
enum class Choice {\n\
    ENTRY {\n\
        override fun value(): Int = 7\n\
        fun use(): Int = value()\n\
    };\n\
    abstract fun value(): Int\n\
}\n";
    let tokens = lex(source, &mut diagnostics);
    let file = parse(source, &tokens, &mut diagnostics);
    let files = vec![file];
    let mut symbols = collect_signatures(&files, &mut diagnostics);
    let info = check_file(&files[0], &mut symbols, &mut diagnostics);
    assert!(
        diagnostics.diags.is_empty(),
        "{:?}",
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| &diagnostic.msg)
            .collect::<Vec<_>>()
    );
    let entry = crate::types::type_name("sample/Choice").nested_child("ENTRY");
    let owners: Vec<_> = info
        .resolved_calls
        .values()
        .filter_map(|call| match call {
            ResolvedCall::Member(member) if member.member.name == "value" => member.member.owner,
            _ => None,
        })
        .collect();
    assert_eq!(owners, vec![entry]);
}
