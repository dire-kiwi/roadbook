//! The main loop: messages in, the world updated, answers and diagnostics out.

// `WorkspaceEdit::changes` is keyed by `Uri`, whose parsed parts are cached in cells; the
// cache does not take part in hashing or equality.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;

use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, DocumentFormattingParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, HoverProviderCapability, InitializeParams, Location,
    MarkupContent, MarkupKind, NumberOrString, OneOf, Position, PositionEncodingKind,
    PrepareRenameResponse, PublishDiagnosticsParams, ReferenceParams, RenameOptions, RenameParams,
    SaveOptions, ServerCapabilities, ServerInfo, TextDocumentIdentifier,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit, Uri, WorkspaceEdit,
};
use roadbook_compiler::{FileId, Severity, Span, SymbolId};
use serde_json::Value;

use crate::FindProject;
use crate::outline::outline;
use crate::rename;
use crate::text::{Encoding, Lines, uri_path};
use crate::world::{Analysis, Doc, World, key, memory_key};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Runs the server on a connection until the client asks it to exit.
pub fn run(connection: Connection, find: FindProject) -> Result<()> {
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let utf8 = params
        .capabilities
        .general
        .as_ref()
        .and_then(|g| g.position_encodings.as_ref())
        .is_some_and(|e| e.contains(&PositionEncodingKind::UTF8));
    let enc = if utf8 {
        Encoding::Utf8
    } else {
        Encoding::Utf16
    };
    let capabilities = ServerCapabilities {
        position_encoding: Some(if utf8 {
            PositionEncodingKind::UTF8
        } else {
            PositionEncodingKind::UTF16
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::FULL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
                ..TextDocumentSyncOptions::default()
            },
        )),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        ..ServerCapabilities::default()
    };
    let result = serde_json::json!({
        "capabilities": capabilities,
        "serverInfo": ServerInfo {
            name: "roadbook".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        },
    });
    connection.initialize_finish(id, result)?;
    // `lsp_server`'s standard input reader hands over each message only when it is taken
    // (its channel has no room), so nothing would ever be queued behind a compilation. A
    // thread takes every message as it arrives and queues it here, without bound, so that
    // the changes made while a compilation runs are all taken in before the next one.
    let (queue, inbox) = mpsc::channel();
    let incoming = connection.receiver.clone();
    std::thread::spawn(move || {
        for msg in incoming {
            if queue.send(msg).is_err() {
                break;
            }
        }
    });
    let mut server = Server {
        connection: &connection,
        inbox,
        world: World::new(find),
        enc,
    };
    while let Ok(msg) = server.inbox.recv() {
        if server.handle(msg)? {
            return Ok(());
        }
        // Take in every message already queued before compiling.
        while let Ok(msg) = server.inbox.try_recv() {
            if server.handle(msg)? {
                return Ok(());
            }
        }
        server.flush();
    }
    Ok(())
}

struct Server<'c> {
    connection: &'c Connection,
    /// The client's messages, in order.
    inbox: mpsc::Receiver<Message>,
    world: World,
    enc: Encoding,
}

/// How the server identifies a document: the file's [`key`], or for a document that is not
/// a file (an untitled buffer) a key of its own.
fn doc_key(uri: &Uri) -> Option<PathBuf> {
    match uri_path(uri) {
        Some(path) => Some(key(&path)),
        None if uri.as_str().starts_with("file:") => None,
        None => Some(memory_key(uri)),
    }
}

/// The parameters of `roadbook/ir`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IrParams {
    text_document: TextDocumentIdentifier,
}

/// An error answer: the request cannot be done, with a reason for the user.
struct Failed(String);

type Answer = std::result::Result<Value, Failed>;

fn params<P: serde::de::DeserializeOwned>(v: Value) -> std::result::Result<P, Failed> {
    serde_json::from_value(v).map_err(|e| Failed(format!("invalid parameters: {e}")))
}

fn json(v: impl serde::Serialize) -> Answer {
    Ok(serde_json::to_value(v).unwrap_or(Value::Null))
}

impl Server<'_> {
    /// Handles one message; `true` once the client has asked to exit.
    fn handle(&mut self, msg: Message) -> Result<bool> {
        match msg {
            Message::Request(req) => {
                if req.method == "shutdown" {
                    self.respond(req.id, Some(Ok(Value::Null)));
                    return Ok(self.await_exit());
                }
                self.flush();
                let id = req.id.clone();
                let answer = self.request(req);
                self.respond(id, answer);
            }
            Message::Notification(n) => self.notification(n),
            Message::Response(_) => {}
        }
        Ok(false)
    }

    /// After `shutdown`: waits for `exit`, refusing any request meanwhile. `true` once it
    /// comes, or the client is gone.
    fn await_exit(&self) -> bool {
        while let Ok(msg) = self.inbox.recv() {
            match msg {
                Message::Notification(n) if n.method == "exit" => return true,
                Message::Request(r) => {
                    let response = Response::new_err(
                        r.id,
                        ErrorCode::InvalidRequest as i32,
                        "the server is shutting down".into(),
                    );
                    let _ = self.connection.sender.send(response.into());
                }
                _ => {}
            }
        }
        true
    }

    fn respond(&self, id: RequestId, answer: Option<Answer>) {
        let response = match answer {
            Some(Ok(v)) => Response::new_ok(id, v),
            Some(Err(Failed(why))) => Response::new_err(id, ErrorCode::RequestFailed as i32, why),
            None => Response::new_err(id, ErrorCode::MethodNotFound as i32, "not supported".into()),
        };
        let _ = self.connection.sender.send(response.into());
    }

    fn request(&mut self, req: Request) -> Option<Answer> {
        let p = req.params;
        Some(match req.method.as_str() {
            "textDocument/definition" => params(p).and_then(|p| self.definition(p)),
            "textDocument/references" => params(p).and_then(|p| self.references(p)),
            "textDocument/prepareRename" => params(p).and_then(|p| self.prepare_rename(p)),
            "textDocument/rename" => params(p).and_then(|p| self.rename(p)),
            "textDocument/hover" => params(p).and_then(|p| self.hover(p)),
            "textDocument/formatting" => params(p).and_then(|p| self.format(p)),
            "textDocument/documentSymbol" => params(p).and_then(|p| self.symbols(p)),
            "textDocument/codeAction" => params(p).and_then(|p| self.code_actions(p)),
            "roadbook/ir" => params(p).and_then(|p| self.ir(p)),
            _ => return None,
        })
    }

    fn notification(&mut self, n: Notification) {
        match n.method.as_str() {
            "textDocument/didOpen" => {
                if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params)
                    && let Some(path) = doc_key(&p.text_document.uri)
                {
                    let known = self.world.analyses.contains_key(&path);
                    self.world.docs.insert(
                        path.clone(),
                        Doc {
                            uri: p.text_document.uri,
                            text: p.text_document.text,
                            version: p.text_document.version,
                        },
                    );
                    self.world.touch(path, !known);
                }
            }
            "textDocument/didChange" => {
                if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params)
                    && let Some(path) = doc_key(&p.text_document.uri)
                {
                    if let Some(doc) = self.world.docs.get_mut(&path) {
                        // Full synchronisation: the last change is the whole text.
                        if let Some(c) = p.content_changes.into_iter().last() {
                            doc.text = c.text;
                        }
                        doc.version = p.text_document.version;
                        self.world.touch(path, false);
                    }
                }
            }
            "textDocument/didSave" => {
                if let Ok(p) = serde_json::from_value::<DidSaveTextDocumentParams>(n.params)
                    && let Some(path) = doc_key(&p.text_document.uri)
                {
                    self.world.touch(path, true);
                }
            }
            "textDocument/didClose" => {
                if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params)
                    && let Some(path) = doc_key(&p.text_document.uri)
                {
                    self.world.docs.remove(&path);
                    self.world.touch(path, false);
                }
            }
            "workspace/didChangeWatchedFiles" => {
                if let Ok(p) = serde_json::from_value::<DidChangeWatchedFilesParams>(n.params) {
                    for change in p.changes {
                        if let Some(path) = uri_path(&change.uri) {
                            self.world.touch(key(&path), true);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Compiles what changed, and publishes the diagnostics of every file compiled.
    fn flush(&mut self) {
        if !self.world.is_dirty() {
            return;
        }
        let (analyses, gone) = self.world.flush();
        for path in gone {
            self.publish(&path, Vec::new());
        }
        for a in analyses {
            let mut by_file: HashMap<FileId, Vec<Diagnostic>> = HashMap::new();
            for d in &a.out.diagnostics {
                if let Some(lsp) = self.diagnostic(&a, d) {
                    by_file.entry(d.file).or_default().push(lsp);
                }
            }
            for (i, path) in a.paths.iter().enumerate() {
                let Some(path) = path else { continue };
                let file = FileId(i as u32);
                let diags = by_file.remove(&file).unwrap_or_default();
                if a.roots.contains(path) || self.world.docs.contains_key(path) || !diags.is_empty()
                {
                    self.publish(path, diags);
                }
            }
        }
    }

    fn publish(&self, path: &Path, diagnostics: Vec<Diagnostic>) {
        let params = PublishDiagnosticsParams {
            uri: self.world.uri(path),
            diagnostics,
            version: self.world.docs.get(path).map(|d| d.version),
        };
        let n = Notification::new("textDocument/publishDiagnostics".into(), params);
        let _ = self.connection.sender.send(n.into());
    }

    fn diagnostic(&self, a: &Analysis, d: &roadbook_compiler::Diagnostic) -> Option<Diagnostic> {
        let range = Lines::new(a.text(d.file)).range(d.span, self.enc);
        let mut message = d.message.clone();
        if let Some(label) = &d.label {
            message.push_str(&format!("\n{label}"));
        }
        if let Some(help) = &d.help {
            message.push_str(&format!("\nhelp: {help}"));
        }
        let related: Vec<DiagnosticRelatedInformation> = d
            .labels
            .iter()
            .filter_map(|l| {
                Some(DiagnosticRelatedInformation {
                    location: self.location(a, l.span)?,
                    message: l.message.clone(),
                })
            })
            .collect();
        Some(Diagnostic {
            range,
            severity: Some(match d.severity {
                Severity::Error => DiagnosticSeverity::ERROR,
                Severity::Warning => DiagnosticSeverity::WARNING,
            }),
            code: Some(NumberOrString::String(d.code.to_string())),
            source: Some("roadbook".into()),
            message,
            related_information: (!related.is_empty()).then_some(related),
            ..Diagnostic::default()
        })
    }

    fn location(&self, a: &Analysis, span: Span) -> Option<Location> {
        let path = a.path(span.file)?;
        let range = Lines::new(a.text(span.file)).range(span, self.enc);
        Some(Location::new(self.world.uri(path), range))
    }

    /// The analysis of a document, the file's id in it and the byte offset of a position.
    fn at(&self, uri: &Uri, pos: Position) -> Option<(Rc<Analysis>, FileId, u32)> {
        let path = doc_key(uri)?;
        let a = self.world.analyses.get(&path)?.clone();
        let file = a.file_id(&path)?;
        let offset = Lines::new(a.text(file)).offset(pos, self.enc);
        Some((a, file, offset as u32))
    }

    /// The symbol at a position, and the span of its name there.
    fn symbol_at(&self, p: &TextDocumentPositionParams) -> Option<(Rc<Analysis>, SymbolId, Span)> {
        let (a, file, offset) = self.at(&p.text_document.uri, p.position)?;
        let (id, span) = a.out.index.at(file, offset)?;
        Some((a, id, span))
    }

    fn definition(&self, p: GotoDefinitionParams) -> Answer {
        let found = self
            .symbol_at(&p.text_document_position_params)
            .and_then(|(a, id, _)| self.location(&a, a.out.index.symbol(id).span));
        json(found.map(GotoDefinitionResponse::Scalar))
    }

    fn references(&self, p: ReferenceParams) -> Answer {
        let Some((a, id, _)) = self.symbol_at(&p.text_document_position) else {
            return Ok(Value::Null);
        };
        let index = &a.out.index;
        let decl = p.context.include_declaration.then(|| index.symbol(id).span);
        let locations: Vec<Location> = decl
            .into_iter()
            .chain(index.references_to(id))
            .filter_map(|s| self.location(&a, s))
            .collect();
        json(locations)
    }

    fn prepare_rename(&self, p: TextDocumentPositionParams) -> Answer {
        let Some((a, id, span)) = self.symbol_at(&p) else {
            return Ok(Value::Null);
        };
        rename::renameable(&a, id).map_err(Failed)?;
        let range = Lines::new(a.text(span.file)).range(span, self.enc);
        json(PrepareRenameResponse::RangeWithPlaceholder {
            range,
            placeholder: a.out.index.symbol(id).name.clone(),
        })
    }

    fn rename(&self, p: RenameParams) -> Answer {
        let Some((a, id, _)) = self.symbol_at(&p.text_document_position) else {
            return Err(Failed("there is no name to rename here".into()));
        };
        let edits = rename::rename(&self.world, &a, id, &p.new_name).map_err(Failed)?;
        let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
        for (path, spans) in edits {
            let Some(file) = a.file_id(&path) else {
                continue;
            };
            let lines = Lines::new(a.text(file));
            let edits = spans
                .into_iter()
                .map(|s| TextEdit::new(lines.range(s, self.enc), p.new_name.clone()))
                .collect();
            changes.insert(self.world.uri(&path), edits);
        }
        json(WorkspaceEdit::new(changes))
    }

    fn hover(&self, p: HoverParams) -> Answer {
        let Some((a, id, span)) = self.symbol_at(&p.text_document_position_params) else {
            return Ok(Value::Null);
        };
        let index = &a.out.index;
        let s = index.symbol(id);
        let mut text = format!("```roadbook\n{}\n```", s.signature);
        match s.parent {
            Some(parent) => text.push_str(&format!(
                "\n\n{} of `{}`",
                s.kind.describe(),
                index.qualified_name(parent)
            )),
            None => {
                if let Some(path) = a.path(s.span.file) {
                    let shown = path
                        .strip_prefix(&a.project)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    text.push_str(&format!("\n\n{} in `{shown}`", s.kind.describe()));
                }
            }
        }
        if let Some(doc) = &s.doc {
            text.push_str(&format!("\n\n---\n\n{doc}"));
        }
        let range = Lines::new(a.text(span.file)).range(span, self.enc);
        json(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: text,
            }),
            range: Some(range),
        })
    }

    /// `roadbook/ir`: the document compiled as an entry, as `roadbook export ir` would
    /// compile it, with the open documents as they are in the editor.
    fn ir(&mut self, p: IrParams) -> Answer {
        let uri = p.text_document.uri;
        let Some(path) = doc_key(&uri) else {
            return Err(Failed(format!("`{}` is not a local file", uri.as_str())));
        };
        let compiled = self.world.entry_ir(&path).map_err(Failed)?;
        let out = &compiled.out;
        let diagnostics: Vec<Value> = out
            .diagnostics
            .iter()
            .map(|d| roadbook_compiler::json::diagnostic(d, &out.files))
            .collect();
        Ok(serde_json::json!({
            "uri": uri.as_str(),
            "version": self.world.docs.get(&path).map(|d| d.version),
            "root": compiled.root.map(|r| r.to_string_lossy().into_owned()),
            "entry": compiled.entry,
            "ir": out.ir.as_ref().map(|ir| ir.to_json()),
            "diagnostics": diagnostics,
        }))
    }

    fn document_text(&self, doc: &TextDocumentIdentifier) -> Option<String> {
        self.world.text(&doc_key(&doc.uri)?)
    }

    fn format(&self, p: DocumentFormattingParams) -> Answer {
        let Some(text) = self.document_text(&p.text_document) else {
            return Ok(Value::Null);
        };
        match roadbook_syntax::format(&text) {
            Ok(formatted) if formatted == text => json(Vec::<TextEdit>::new()),
            Ok(formatted) => {
                let all = Lines::new(&text).all(self.enc);
                json(vec![TextEdit::new(all, formatted)])
            }
            // A file with syntax errors is not formatted; its diagnostics say why.
            Err(_) => Ok(Value::Null),
        }
    }

    fn symbols(&self, p: DocumentSymbolParams) -> Answer {
        let Some(text) = self.document_text(&p.text_document) else {
            return Ok(Value::Null);
        };
        json(DocumentSymbolResponse::Nested(outline(&text, self.enc)))
    }

    fn code_actions(&self, p: CodeActionParams) -> Answer {
        let Some(path) = doc_key(&p.text_document.uri) else {
            return Ok(Value::Null);
        };
        let Some(a) = self.world.analyses.get(&path) else {
            return json(Vec::<CodeActionOrCommand>::new());
        };
        let Some(file) = a.file_id(&path) else {
            return json(Vec::<CodeActionOrCommand>::new());
        };
        let lines = Lines::new(a.text(file));
        let (start, end) = (
            lines.offset(p.range.start, self.enc) as u32,
            lines.offset(p.range.end, self.enc) as u32,
        );
        let mut actions = Vec::new();
        for d in &a.out.diagnostics {
            if d.file != file || d.span.start.offset > end || start > d.span.end.offset {
                continue;
            }
            let lsp = self.diagnostic(a, d);
            for fix in &d.fixes {
                let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
                for e in &fix.edits {
                    let Some(path) = a.path(e.span.file) else {
                        continue;
                    };
                    let range = Lines::new(a.text(e.span.file)).range(e.span, self.enc);
                    changes
                        .entry(self.world.uri(path))
                        .or_default()
                        .push(TextEdit::new(range, e.text.clone()));
                }
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.message.clone(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: lsp.clone().map(|d| vec![d]),
                    edit: Some(WorkspaceEdit::new(changes)),
                    is_preferred: Some(d.fixes.len() == 1),
                    ..CodeAction::default()
                }));
            }
        }
        json(actions)
    }
}
