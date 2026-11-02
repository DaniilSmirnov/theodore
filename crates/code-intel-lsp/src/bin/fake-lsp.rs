//! A scripted language server for the crate's tests. Speaks LSP over stdio
//! and derives its answers from the open documents' text:
//!
//! | request | answer |
//! | --- | --- |
//! | definition | first occurrence of the word under the cursor in that file |
//! | references | every occurrence in every open file |
//! | documentSymbol | `struct X` lines, with indented `fn y` lines as children |
//! | workspace/symbol | the same, flat, across open files |
//! | rename | every occurrence in every open file |
//! | hover | JSON of what the client told it (config answer, folders, …) |
//! | diagnostics | an error on each `bad` |
//!
//! Positions count UTF-16 units unless `FAKE_LSP_ENCODING=utf-8`.
//!
//! Environment:
//! `FAKE_LSP_PROGRESS_MS` (indexing token for that long; `never`),
//! `FAKE_LSP_CRASH_ON` / `FAKE_LSP_HANG_ON` (a method),
//! `FAKE_LSP_FLAT_SYMBOLS`, `FAKE_LSP_RENAME_CHANGES`, `FAKE_LSP_PULL`,
//! `FAKE_LSP_FOLDERS`, `FAKE_LSP_IGNORE_EXIT`, `FAKE_LSP_PID_FILE`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, msg: Value) {
    let body = msg.to_string();
    let mut o = out.lock().unwrap();
    let _ = write!(o, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = o.flush();
}

fn read(input: &mut impl BufRead) -> Option<Value> {
    let mut len = 0;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

struct Server {
    out: Out,
    utf8: bool,
    docs: BTreeMap<String, String>,
    folders: Vec<Value>,
    init_options: Value,
    config_answer: Value,
    watched: usize,
    next_id: i64,
}

impl Server {
    fn units(&self, c: char) -> u32 {
        if self.utf8 {
            c.len_utf8() as u32
        } else {
            c.len_utf16() as u32
        }
    }

    /// Whole-word occurrences of `word`: (line, start, end) in units.
    fn find(&self, text: &str, word: &str) -> Vec<(u32, u32, u32)> {
        let mut hits = vec![];
        for (ln, line) in text.split('\n').enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let w: Vec<char> = word.chars().collect();
            let mut i = 0;
            while i + w.len() <= chars.len() {
                let before = i == 0 || !is_ident(chars[i - 1]);
                let after = i + w.len() == chars.len() || !is_ident(chars[i + w.len()]);
                if before && after && chars[i..i + w.len()] == w[..] {
                    let start: u32 = chars[..i].iter().map(|c| self.units(*c)).sum();
                    let end = start + w.iter().map(|c| self.units(*c)).sum::<u32>();
                    hits.push((ln as u32, start, end));
                    i += w.len();
                } else {
                    i += 1;
                }
            }
        }
        hits
    }

    fn word_at(&self, uri: &str, pos: &Value) -> Option<String> {
        let text = self.docs.get(uri)?;
        let line = text.split('\n').nth(pos["line"].as_u64()? as usize)?;
        let target = pos["character"].as_u64()? as u32;
        let chars: Vec<char> = line.chars().collect();
        let mut units = 0;
        let mut ix = chars.len();
        for (i, c) in chars.iter().enumerate() {
            if units >= target {
                ix = i;
                break;
            }
            units += self.units(*c);
        }
        if ix >= chars.len() || !is_ident(chars[ix]) {
            return None;
        }
        let mut s = ix;
        while s > 0 && is_ident(chars[s - 1]) {
            s -= 1;
        }
        let mut e = ix;
        while e < chars.len() && is_ident(chars[e]) {
            e += 1;
        }
        Some(chars[s..e].iter().collect())
    }

    fn range(l: u32, s: u32, e: u32) -> Value {
        json!({"start": {"line": l, "character": s}, "end": {"line": l, "character": e}})
    }

    fn occurrences(&self, word: &str) -> Vec<(String, Value)> {
        let mut out = vec![];
        for (uri, text) in &self.docs {
            for (l, s, e) in self.find(text, word) {
                out.push((uri.clone(), Self::range(l, s, e)));
            }
        }
        out
    }

    fn diagnostics(&self, text: &str) -> Value {
        Value::Array(
            self.find(text, "bad")
                .into_iter()
                .map(|(l, s, e)| {
                    json!({"range": Self::range(l, s, e), "severity": 1, "code": "E1",
                           "message": "bad word"})
                })
                .collect(),
        )
    }

    fn publish(&self, uri: &str) {
        if env("FAKE_LSP_PULL").is_some() {
            return;
        }
        let text = self.docs.get(uri).cloned().unwrap_or_default();
        send(
            &self.out,
            json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                   "params": {"uri": uri, "diagnostics": self.diagnostics(&text)}}),
        );
    }

    /// `(name, kind, line, name start, name end, line end, children)`.
    fn symbols(&self, text: &str) -> Vec<Value> {
        let mut top: Vec<Value> = vec![];
        for (ln, line) in text.split('\n').enumerate() {
            let ln = ln as u32;
            let trimmed = line.trim_start();
            let indented = trimmed.len() != line.len();
            let (kw, kind) = if trimmed.starts_with("struct ") {
                ("struct ", 23)
            } else if trimmed.starts_with("fn ") {
                ("fn ", if indented { 6 } else { 12 })
            } else {
                continue;
            };
            let name: String = trimmed[kw.len()..]
                .chars()
                .take_while(|c| is_ident(*c))
                .collect();
            let lead: u32 = line[..line.len() - trimmed.len()]
                .chars()
                .map(|c| self.units(c))
                .sum();
            let start = lead + kw.len() as u32;
            let end = start + name.chars().map(|c| self.units(c)).sum::<u32>();
            let line_end: u32 = line.chars().map(|c| self.units(c)).sum();
            let sym = json!({
                "name": name, "kind": kind,
                "range": {"start": {"line": ln, "character": 0},
                          "end": {"line": ln, "character": line_end}},
                "selectionRange": Self::range(ln, start, end),
                "children": [],
            });
            match top.last_mut() {
                Some(parent) if indented && parent["kind"] == 23 => {
                    parent["children"].as_array_mut().unwrap().push(sym);
                    parent["range"]["end"] = json!({"line": ln, "character": line_end});
                }
                _ => top.push(sym),
            }
        }
        top
    }

    fn flat(uri: &str, syms: &[Value], container: Option<&str>, out: &mut Vec<Value>) {
        for s in syms {
            let mut info = json!({
                "name": s["name"], "kind": s["kind"],
                "location": {"uri": uri, "range": s["selectionRange"]},
            });
            if let Some(c) = container {
                info["containerName"] = json!(c);
            }
            out.push(info);
            Self::flat(
                uri,
                s["children"].as_array().unwrap(),
                s["name"].as_str(),
                out,
            );
        }
    }

    fn request(&mut self, method: &str, p: &Value) -> Value {
        let uri = p["textDocument"]["uri"].as_str().unwrap_or("").to_string();
        match method {
            "initialize" => {
                self.init_options = p["initializationOptions"].clone();
                self.folders = p["workspaceFolders"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let mut caps = json!({
                    "textDocumentSync": 1,
                    "definitionProvider": true,
                    "referencesProvider": true,
                    "documentSymbolProvider": true,
                    "workspaceSymbolProvider": true,
                    "renameProvider": true,
                    "hoverProvider": true,
                });
                if !self.utf8 {
                    caps["positionEncoding"] = json!("utf-16");
                } else {
                    caps["positionEncoding"] = json!("utf-8");
                }
                if env("FAKE_LSP_PULL").is_some() {
                    caps["diagnosticProvider"] =
                        json!({"interFileDependencies": false, "workspaceDiagnostics": false});
                }
                if env("FAKE_LSP_FOLDERS").is_some() {
                    caps["workspace"] = json!({"workspaceFolders":
                        {"supported": true, "changeNotifications": true}});
                }
                json!({"capabilities": caps, "serverInfo": {"name": "fake-lsp"}})
            }
            "shutdown" => Value::Null,
            "textDocument/definition" => {
                let Some(word) = self.word_at(&uri, &p["position"]) else {
                    return Value::Null;
                };
                let text = &self.docs[&uri];
                let mut locs: Vec<Value> = self
                    .find(text, &word)
                    .into_iter()
                    .take(1)
                    .map(|(l, s, e)| json!({"uri": uri, "range": Self::range(l, s, e)}))
                    .collect();
                locs.push(json!({"uri": "file:///outside/lib.rs",
                                 "range": Self::range(0, 0, 1)}));
                Value::Array(locs)
            }
            "textDocument/references" => {
                let Some(word) = self.word_at(&uri, &p["position"]) else {
                    return json!([]);
                };
                let skip = usize::from(p["context"]["includeDeclaration"] == false);
                Value::Array(
                    self.occurrences(&word)
                        .into_iter()
                        .skip(skip)
                        .map(|(uri, range)| json!({"uri": uri, "range": range}))
                        .collect(),
                )
            }
            "textDocument/documentSymbol" => {
                let syms = self.symbols(self.docs.get(&uri).map(String::as_str).unwrap_or(""));
                if env("FAKE_LSP_FLAT_SYMBOLS").is_some() {
                    let mut out = vec![];
                    Self::flat(&uri, &syms, None, &mut out);
                    Value::Array(out)
                } else {
                    Value::Array(syms)
                }
            }
            "workspace/symbol" => {
                let q = p["query"].as_str().unwrap_or("");
                let mut out = vec![];
                for (uri, text) in &self.docs {
                    Self::flat(uri, &self.symbols(text), None, &mut out);
                }
                out.retain(|s| s["name"].as_str().unwrap_or("").contains(q));
                Value::Array(out)
            }
            "textDocument/rename" => {
                let Some(word) = self.word_at(&uri, &p["position"]) else {
                    return Value::Null;
                };
                let new = p["newName"].clone();
                let mut by_uri: BTreeMap<String, Vec<Value>> = BTreeMap::new();
                for (u, range) in self.occurrences(&word) {
                    by_uri
                        .entry(u)
                        .or_default()
                        .push(json!({"range": range, "newText": new}));
                }
                if env("FAKE_LSP_RENAME_CHANGES").is_some() {
                    json!({"changes": by_uri})
                } else {
                    let docs: Vec<Value> = by_uri
                        .into_iter()
                        .map(|(u, edits)| {
                            json!({"textDocument": {"uri": u, "version": 0}, "edits": edits})
                        })
                        .collect();
                    json!({"documentChanges": docs})
                }
            }
            "textDocument/hover" => {
                let report = json!({
                    "config": self.config_answer,
                    "init_options": self.init_options,
                    "folders": self.folders.iter().map(|f| f["uri"].clone()).collect::<Vec<_>>(),
                    "open": self.docs.keys().collect::<Vec<_>>(),
                    "watched": self.watched,
                });
                json!({"contents": {"kind": "markdown", "value": report.to_string()}})
            }
            "textDocument/diagnostic" => {
                let text = self.docs.get(&uri).cloned().unwrap_or_default();
                json!({"kind": "full", "items": self.diagnostics(&text)})
            }
            _ => Value::Null,
        }
    }

    fn notification(&mut self, method: &str, p: &Value) {
        let uri = p["textDocument"]["uri"].as_str().unwrap_or("").to_string();
        match method {
            "initialized" => self.initialized(),
            "exit" => {
                if env("FAKE_LSP_IGNORE_EXIT").is_none() {
                    std::process::exit(0);
                }
            }
            "textDocument/didOpen" => {
                let text = p["textDocument"]["text"].as_str().unwrap_or("").to_string();
                self.docs.insert(uri.clone(), text);
                self.publish(&uri);
            }
            "textDocument/didChange" => {
                let text = p["contentChanges"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                self.docs.insert(uri.clone(), text);
                self.publish(&uri);
            }
            "textDocument/didClose" => {
                self.docs.remove(&uri);
            }
            "workspace/didChangeWatchedFiles" => {
                self.watched += p["changes"].as_array().map_or(0, Vec::len);
            }
            "workspace/didChangeWorkspaceFolders" => {
                let ev = &p["event"];
                for r in ev["removed"].as_array().into_iter().flatten() {
                    self.folders.retain(|f| f["uri"] != r["uri"]);
                }
                for a in ev["added"].as_array().into_iter().flatten() {
                    self.folders.push(a.clone());
                }
            }
            _ => {}
        }
    }

    fn call(&mut self, method: &str, params: Value) -> i64 {
        self.next_id += 1;
        send(
            &self.out,
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}),
        );
        self.next_id
    }

    fn initialized(&mut self) {
        self.call(
            "client/registerCapability",
            json!({"registrations": [{"id": "w", "method": "workspace/didChangeWatchedFiles",
                   "registerOptions": {"watchers": [{"globPattern": "**/*"}]}}]}),
        );
        self.call(
            "workspace/configuration",
            json!({"items": [{"section": "fake"}, {"section": "fake.nested.key"},
                             {"section": "fake-lsp.top"}, {"section": "other"}]}),
        );
        if env("FAKE_LSP_PROGRESS_MS").is_some() {
            self.call("window/workDoneProgress/create", json!({"token": "index"}));
        }
    }

    /// The client answered `id`.
    fn response(&mut self, msg: &Value) {
        let result = &msg["result"];
        if result.is_array() {
            self.config_answer = result.clone();
        }
        if msg["id"] == json!(3) {
            // The progress token exists now.
            send(
                &self.out,
                json!({"jsonrpc": "2.0", "method": "$/progress", "params":
                       {"token": "index", "value": {"kind": "begin", "title": "Indexing"}}}),
            );
            let ms = env("FAKE_LSP_PROGRESS_MS").unwrap_or_default();
            if let Ok(ms) = ms.parse::<u64>() {
                let out = self.out.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(ms));
                    send(
                        &out,
                        json!({"jsonrpc": "2.0", "method": "$/progress", "params":
                               {"token": "index", "value": {"kind": "end"}}}),
                    );
                });
            }
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn main() {
    if let Some(f) = env("FAKE_LSP_PID_FILE") {
        let _ = std::fs::write(f, std::process::id().to_string());
    }
    let crash_on = env("FAKE_LSP_CRASH_ON");
    let hang_on = env("FAKE_LSP_HANG_ON");
    let mut server = Server {
        out: Arc::new(Mutex::new(std::io::stdout())),
        utf8: env("FAKE_LSP_ENCODING").as_deref() == Some("utf-8"),
        docs: BTreeMap::new(),
        folders: vec![],
        init_options: Value::Null,
        config_answer: Value::Null,
        watched: 0,
        next_id: 0,
    };
    let mut input = BufReader::new(std::io::stdin());
    while let Some(msg) = read(&mut input) {
        let method = msg["method"].as_str().map(str::to_string);
        let Some(method) = method else {
            server.response(&msg);
            continue;
        };
        if crash_on.as_deref() == Some(method.as_str()) {
            std::process::exit(3);
        }
        if hang_on.as_deref() == Some(method.as_str()) {
            continue;
        }
        let params = msg["params"].clone();
        match msg.get("id") {
            Some(id) => {
                let result = server.request(&method, &params);
                send(
                    &server.out,
                    json!({"jsonrpc": "2.0", "id": id, "result": result}),
                );
            }
            None => server.notification(&method, &params),
        }
    }
}
