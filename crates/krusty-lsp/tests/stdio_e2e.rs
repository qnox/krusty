use std::io::{BufReader, Write};
use std::process::{Command, Stdio};

use krusty_lsp::{read_framed, write_framed, MAX_MESSAGE_BYTES};
use serde_json::{json, Value};

fn run_server(arguments: &[&str], messages: &[Value]) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_krusty-lsp"));
    command.args(["--stdio", "-no-jdk"]).args(arguments);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start krusty-lsp");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for message in messages {
            write_framed(stdin, &serde_json::to_vec(message).unwrap()).unwrap();
        }
        stdin.flush().unwrap();
    }
    drop(child.stdin.take());

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut output = Vec::new();
    while let Some(body) = read_framed(&mut stdout, MAX_MESSAGE_BYTES).unwrap() {
        output.push(serde_json::from_slice(&body).unwrap());
    }
    assert!(child.wait().unwrap().success());
    output
}

#[test]
fn stdio_server_uses_the_compiler_worker_and_exits_cleanly() {
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///main.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": "fun answer(): Int = 42\n\
                             fun box(): Int = \"no\"\n\
                             fun use(): Int = ans\n\
                             fun navigate(): Int = answer()"
                }
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 2, "character": 20}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/definition",
            "params": {
                "textDocument": {"uri": "file:///main.kt"},
                "position": {"line": 3, "character": 23}
            }
        }),
        json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&[], &messages);
    assert_eq!(output[0]["id"], 1);
    assert_eq!(output[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        output[1]["params"]["diagnostics"][0]["message"],
        "Return type mismatch: expected 'Int', actual 'String'."
    );
    assert_eq!(output[2]["id"], 2);
    assert!(output[2]["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["label"] == "answer" && item["kind"] == 3));
    assert_eq!(output[3]["id"], 3);
    assert_eq!(
        output[3]["result"],
        json!([{
            "uri": "file:///main.kt",
            "range": {
                "start": {"line": 0, "character": 4},
                "end": {"line": 0, "character": 10}
            }
        }])
    );
    assert_eq!(output[4]["id"], 4);
}

#[test]
fn stdio_server_applies_configured_language_features() {
    let source = "\
data class Entry(val first: String, val second: String)
fun combine(entries: Array<Entry>): String {
    var result = \"\"
    for ([left, right] in entries) {
        result += left + right
    }
    return result
}";
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///feature.kt",
                    "languageId": "kotlin",
                    "version": 1,
                    "text": source
                }
            }
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": null}),
        json!({"jsonrpc": "2.0", "method": "exit", "params": null}),
    ];
    let output = run_server(&["-Xname-based-destructuring=complete"], &messages);
    let diagnostics = output
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .and_then(|message| message["params"]["diagnostics"].as_array())
        .expect("published diagnostics");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
