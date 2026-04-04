//! QuickLSP server implementation.
//!
//! All LSP operations go through a single `Workspace` engine, with
//! a `DependencyIndex` as fallback for symbols from external packages.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::{Client, LanguageServer};

use crate::deps::DependencyIndex;
use crate::parsing::symbols::{self, SymbolKind as QuickSymbolKind};
use crate::workspace::{SymbolLocation, Workspace};

pub struct QuickLspServer {
    client: Client,
    workspace: Arc<Workspace>,
    dep_index: Arc<DependencyIndex>,
    workspace_root: Arc<RwLock<Option<PathBuf>>>,
}

impl QuickLspServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            workspace: Arc::new(Workspace::new()),
            dep_index: Arc::new(DependencyIndex::new()),
            workspace_root: Arc::new(RwLock::new(None)),
        }
    }

    fn word_at_position(content: &str, position: Position) -> Option<String> {
        let line = content.lines().nth(position.line as usize)?;
        let col = position.character as usize;
        let chars: Vec<char> = line.chars().collect();
        if col > chars.len() {
            return None;
        }
        let mut start = col;
        let mut end = col;
        while start > 0 && is_ident_char(chars[start - 1]) {
            start -= 1;
        }
        while end < chars.len() && is_ident_char(chars[end]) {
            end += 1;
        }
        if start == end {
            return None;
        }
        Some(chars[start..end].iter().collect())
    }

    /// Extract the qualifier before the word at cursor position.
    ///
    /// For `Workspace::new()` with cursor on `new`, returns `Some("Workspace")`.
    /// For `self.workspace.scan_directory()` with cursor on `scan_directory`,
    /// returns `Some("workspace")`.
    /// Returns `None` if there is no qualifier (bare identifier).
    fn qualifier_at_position(content: &str, position: Position) -> Option<String> {
        let line = content.lines().nth(position.line as usize)?;
        let col = position.character as usize;
        let chars: Vec<char> = line.chars().collect();
        if col > chars.len() {
            return None;
        }

        // Find start of the current word
        let mut word_start = col;
        while word_start > 0 && is_ident_char(chars[word_start - 1]) {
            word_start -= 1;
        }

        // Check for separator before the word: `::`, `.`, or `->`
        let mut sep_end = word_start;
        if sep_end >= 2 && chars[sep_end - 2] == ':' && chars[sep_end - 1] == ':' {
            sep_end -= 2;
        } else if sep_end >= 2 && chars[sep_end - 2] == '-' && chars[sep_end - 1] == '>' {
            sep_end -= 2;
        } else if sep_end >= 1 && chars[sep_end - 1] == '.' {
            sep_end -= 1;
        } else {
            return None; // No separator — bare identifier
        }

        // Extract the qualifier identifier before the separator
        let mut q_end = sep_end;
        while q_end > 0 && chars[q_end - 1] == ' ' {
            q_end -= 1;
        }
        let mut q_start = q_end;
        while q_start > 0 && is_ident_char(chars[q_start - 1]) {
            q_start -= 1;
        }
        if q_start == q_end {
            return None;
        }
        Some(chars[q_start..q_end].iter().collect())
    }
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_alphanumeric()
}

/// Send a `$/progress` notification to the client.
async fn send_progress(client: &Client, token: &NumberOrString, value: WorkDoneProgress) {
    client
        .send_notification::<notification::Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(value),
        })
        .await;
}

fn aero_kind_to_lsp(kind: QuickSymbolKind) -> SymbolKind {
    match kind {
        QuickSymbolKind::Function => SymbolKind::FUNCTION,
        QuickSymbolKind::Method => SymbolKind::METHOD,
        QuickSymbolKind::Class => SymbolKind::CLASS,
        QuickSymbolKind::Struct => SymbolKind::STRUCT,
        QuickSymbolKind::Enum => SymbolKind::ENUM,
        QuickSymbolKind::Interface => SymbolKind::INTERFACE,
        QuickSymbolKind::Constant => SymbolKind::CONSTANT,
        QuickSymbolKind::Variable => SymbolKind::VARIABLE,
        QuickSymbolKind::Module => SymbolKind::MODULE,
        QuickSymbolKind::TypeAlias => SymbolKind::TYPE_PARAMETER,
        QuickSymbolKind::Trait => SymbolKind::INTERFACE,
        QuickSymbolKind::Unknown => SymbolKind::NULL,
    }
}

fn loc_to_lsp(loc: &SymbolLocation) -> Option<Location> {
    let uri = Url::from_file_path(&loc.file).ok()?;
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: loc.symbol.line as u32,
                character: loc.symbol.col as u32,
            },
            end: Position {
                line: loc.symbol.line as u32,
                character: (loc.symbol.col + loc.symbol.name.len()) as u32,
            },
        },
    })
}

#[tower_lsp::async_trait]
impl LanguageServer for QuickLspServer {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(root_uri) = params.root_uri {
            if let Ok(path) = root_uri.to_file_path() {
                // Detect ecosystems and resolve dependency packages
                self.dep_index.detect_and_resolve(&path);
                *self.workspace_root.write().await = Some(path);
            }
        }
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    ..Default::default()
                }),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), ":".to_string()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "QuickLSP".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "QuickLSP initialized")
            .await;

        // Kick off background indexing: workspace scan first, then dependencies.
        // Both use the same DashMap-based Workspace, so LSP queries work
        // immediately while scanning is in progress.
        let workspace = self.workspace.clone();
        let dep_index = self.dep_index.clone();
        let root = self.workspace_root.read().await.clone();
        let client = self.client.clone();
        tokio::spawn(async move {
            let Some(root) = root else { return };

            let token = NumberOrString::String("quicklsp/indexing".to_string());

            // Create a WorkDoneProgress token with the client.
            // If the client doesn't support it, we fall through gracefully.
            let progress_supported = client
                .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                    token: token.clone(),
                })
                .await
                .is_ok();

            // Phase 1: scan local workspace files (fast — only project sources)
            if progress_supported {
                send_progress(
                    &client,
                    &token,
                    WorkDoneProgress::Begin(WorkDoneProgressBegin {
                        title: "Indexing".to_string(),
                        message: Some("Scanning workspace files...".to_string()),
                        cancellable: Some(false),
                        percentage: Some(0),
                    }),
                )
                .await;
            }

            let ws = workspace.clone();
            let scan_root = root.clone();
            tokio::task::spawn_blocking(move || {
                ws.scan_directory(&scan_root);
            })
            .await
            .ok();

            // Phase 2: index dependency packages with progress reporting.
            // Use a channel so the blocking indexer can report per-package
            // progress back to this async task, which forwards it to the client.
            if progress_supported {
                send_progress(
                    &client,
                    &token,
                    WorkDoneProgress::Report(WorkDoneProgressReport {
                        message: Some("Indexing dependencies...".to_string()),
                        cancellable: Some(false),
                        percentage: Some(0),
                    }),
                )
                .await;
            }

            let (progress_tx, mut progress_rx) =
                tokio::sync::mpsc::unbounded_channel::<(usize, usize)>();

            let dep = dep_index.clone();
            let index_handle = tokio::task::spawn_blocking(move || {
                dep.index_pending(Some(&|done, total| {
                    let _ = progress_tx.send((done, total));
                }));
            });

            // Forward progress to the client via $/progress notifications.
            let progress_client = client.clone();
            let progress_token = token.clone();
            let fwd_supported = progress_supported;
            let progress_handle = tokio::spawn(async move {
                while let Some((done, total)) = progress_rx.recv().await {
                    if fwd_supported {
                        let pct = if total > 0 {
                            ((done as u64 * 100) / total as u64) as u32
                        } else {
                            0
                        };
                        send_progress(
                            &progress_client,
                            &progress_token,
                            WorkDoneProgress::Report(WorkDoneProgressReport {
                                message: Some(format!(
                                    "Indexing dependencies: {done}/{total} packages"
                                )),
                                cancellable: Some(false),
                                percentage: Some(pct),
                            }),
                        )
                        .await;
                    }
                }
            });

            index_handle.await.ok();
            progress_handle.await.ok();

            let done_msg = format!(
                "Indexed {} packages, {} definitions",
                dep_index.package_count(),
                dep_index.definition_count(),
            );

            if progress_supported {
                send_progress(
                    &client,
                    &token,
                    WorkDoneProgress::End(WorkDoneProgressEnd {
                        message: Some(done_msg),
                    }),
                )
                .await;
            } else {
                client.log_message(MessageType::INFO, done_msg).await;
            }
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        if let Ok(path) = params.text_document.uri.to_file_path() {
            self.workspace.index_file(path, params.text_document.text);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.last() {
            if let Ok(path) = params.text_document.uri.to_file_path() {
                self.workspace.update_file(path, change.text.clone());
            }
        }
    }

    async fn did_close(&self, _params: DidCloseTextDocumentParams) {}

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let symbol = match Self::word_at_position(&source, pos) {
            Some(s) => s,
            None => return Ok(None),
        };
        let qualifier = Self::qualifier_at_position(&source, pos);
        let current_file = uri.to_file_path().ok();
        let mut defs = self.workspace.find_definitions(&symbol);
        if defs.is_empty() {
            defs = self.dep_index.find_definitions(&symbol);
        }
        self.workspace
            .rank_definitions(&mut defs, current_file.as_deref(), qualifier.as_deref());
        if let Some(loc) = defs.first().and_then(loc_to_lsp) {
            return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
        }
        Ok(None)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let symbol = match Self::word_at_position(&source, pos) {
            Some(s) => s,
            None => return Ok(None),
        };
        let refs = self.workspace.find_references(&symbol);
        if refs.is_empty() {
            return Ok(None);
        }
        let locs: Vec<Location> = refs
            .iter()
            .filter_map(|r| {
                let u = Url::from_file_path(&r.file).ok()?;
                Some(Location {
                    uri: u,
                    range: Range {
                        start: Position {
                            line: r.line as u32,
                            character: r.col as u32,
                        },
                        end: Position {
                            line: r.line as u32,
                            character: (r.col + r.len) as u32,
                        },
                    },
                })
            })
            .collect();
        if locs.is_empty() {
            Ok(None)
        } else {
            Ok(Some(locs))
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let syms = self.workspace.file_symbols(&path);
        if syms.is_empty() {
            return Ok(None);
        }
        let lsp_syms: Vec<SymbolInformation> = syms
            .iter()
            .map(|s| {
                #[allow(deprecated)]
                SymbolInformation {
                    name: s.name.clone(),
                    kind: aero_kind_to_lsp(s.kind),
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: Range {
                            start: Position {
                                line: s.line as u32,
                                character: s.col as u32,
                            },
                            end: Position {
                                line: s.line as u32,
                                character: (s.col + s.name.len()) as u32,
                            },
                        },
                    },
                    container_name: None,
                }
            })
            .collect();
        Ok(Some(DocumentSymbolResponse::Flat(lsp_syms)))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        if params.query.is_empty() {
            return Ok(None);
        }
        let results = self.workspace.search_symbols(&params.query);
        if results.is_empty() {
            return Ok(None);
        }
        let syms: Vec<SymbolInformation> = results
            .iter()
            .take(20)
            .filter_map(|loc| {
                let location = loc_to_lsp(loc)?;
                #[allow(deprecated)]
                Some(SymbolInformation {
                    name: loc.symbol.name.clone(),
                    kind: aero_kind_to_lsp(loc.symbol.kind),
                    tags: None,
                    deprecated: None,
                    location,
                    container_name: None,
                })
            })
            .collect();
        if syms.is_empty() {
            Ok(None)
        } else {
            Ok(Some(syms))
        }
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let symbol = match Self::word_at_position(&source, pos) {
            Some(s) => s,
            None => return Ok(None),
        };

        // Find definitions and rank them by context (qualifier + same-file)
        let qualifier = Self::qualifier_at_position(&source, pos);
        let current_file = uri.to_file_path().ok();
        let mut defs = self.workspace.find_definitions(&symbol);
        if defs.is_empty() {
            defs = self.dep_index.find_definitions(&symbol);
        }
        self.workspace
            .rank_definitions(&mut defs, current_file.as_deref(), qualifier.as_deref());

        let loc = match defs.first() {
            Some(loc) => loc,
            None => return Ok(None),
        };
        let sig = loc.symbol.signature.clone();
        let doc = loc.symbol.doc_comment.clone();

        // Build markdown hover content: signature as code block + doc as text
        let mut parts = Vec::new();
        if let Some(ref s) = sig {
            parts.push(format!("```\n{s}\n```"));
        }
        if let Some(ref d) = doc {
            parts.push(d.clone());
        }

        if parts.is_empty() {
            return Ok(None);
        }

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: parts.join("\n\n"),
            }),
            range: None,
        }))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };

        let (loc, active_param) = match self.workspace.signature_help_at(
            &source,
            pos.line as usize,
            pos.character as usize,
        ) {
            Some(result) => result,
            // Fallback to dependency index
            None => match self.dep_index.signature_help_at(
                &source,
                pos.line as usize,
                pos.character as usize,
            ) {
                Some(result) => result,
                None => return Ok(None),
            },
        };

        let sig_text = match &loc.symbol.signature {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        let params_list = symbols::extract_parameters(&sig_text);
        let lsp_params: Vec<ParameterInformation> = params_list
            .iter()
            .map(|p| ParameterInformation {
                label: ParameterLabel::Simple(p.clone()),
                documentation: None,
            })
            .collect();

        let sig_info = SignatureInformation {
            label: sig_text,
            documentation: loc.symbol.doc_comment.as_ref().map(|d| {
                Documentation::MarkupContent(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: d.clone(),
                })
            }),
            parameters: if lsp_params.is_empty() {
                None
            } else {
                Some(lsp_params)
            },
            active_parameter: Some(active_param as u32),
        };

        Ok(Some(SignatureHelp {
            signatures: vec![sig_info],
            active_signature: Some(0),
            active_parameter: Some(active_param as u32),
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let partial = match Self::word_at_position(&source, pos) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };
        let mut results = self.workspace.completions(&partial);
        // Merge dependency completions
        let dep_results = self.dep_index.completions(&partial);
        results.extend(dep_results);
        if results.is_empty() {
            return Ok(None);
        }
        let mut seen = std::collections::HashSet::new();
        let items: Vec<CompletionItem> = results
            .into_iter()
            .filter(|loc| seen.insert(loc.symbol.name.clone()))
            .take(20)
            .map(|loc| {
                let detail = loc.symbol.signature.clone();
                let documentation = loc.symbol.doc_comment.as_ref().map(|d| {
                    Documentation::MarkupContent(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: d.clone(),
                    })
                });
                CompletionItem {
                    label: loc.symbol.name,
                    kind: Some(CompletionItemKind::TEXT),
                    detail,
                    documentation,
                    ..Default::default()
                }
            })
            .collect();
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(CompletionResponse::Array(items)))
        }
    }
}
