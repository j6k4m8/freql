//! Exercise the actual stdio handshake: clients only activate providers nested
//! under InitializeResult.capabilities, not a bare ServerCapabilities object.
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn send(writer: &mut impl Write, value: Value) {
    let body = serde_json::to_vec(&value).unwrap();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    writer.write_all(&body).unwrap();
    writer.flush().unwrap();
}

fn receive(reader: &mut impl BufRead) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "server closed before responding"
        );
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.expect("Content-Length header")];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[test]
fn stdio_advertises_editor_capabilities_in_initialize_result() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_freql"))
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut writer = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":null,"rootUri":null,"capabilities":{}}}),
    );
    let initialized = receive(&mut reader);
    assert_eq!(initialized["id"], 1);
    let capabilities = &initialized["result"]["capabilities"];
    for provider in [
        "hoverProvider",
        "definitionProvider",
        "referencesProvider",
        "documentFormattingProvider",
        "codeActionProvider",
    ] {
        assert_eq!(capabilities[provider], true, "{provider}: {initialized}");
    }
    assert_eq!(capabilities["renameProvider"]["prepareProvider"], true);
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}),
    );
    assert_eq!(receive(&mut reader)["id"], 2);
    send(
        &mut writer,
        json!({"jsonrpc":"2.0","method":"exit","params":null}),
    );
    drop(writer);
    assert!(child.wait().unwrap().success());
}
