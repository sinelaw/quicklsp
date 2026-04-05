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

/// Negotiated position encoding for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PosEncoding {
    Utf8,
    Utf16,
    Utf32,
}

/// Convert a byte offset within a line to the client's position encoding.
/// For ASCII lines (the vast majority of code), all encodings produce the
/// same value as the byte offset, so we fast-path that case.
fn byte_col_to_encoding(line: &str, byte_col: usize, encoding: PosEncoding) -> u32 {
    let byte_col = byte_col.min(line.len());
    match encoding {
        PosEncoding::Utf8 => byte_col as u32,
        _ => {
            let prefix = &line.as_bytes()[..byte_col];
            if prefix.is_ascii() {
                return byte_col as u32;
            }
            let prefix_str = &line[..byte_col];
            match encoding {
                PosEncoding::Utf32 => prefix_str.chars().count() as u32,
                PosEncoding::Utf16 => prefix_str.encode_utf16().count() as u32,
                PosEncoding::Utf8 => unreachable!(),
            }
        }
    }
}

/// Convert a client position encoding column back to a byte offset within a line.
/// Used for incoming positions from the client (e.g., cursor position).
fn encoding_col_to_byte(line: &str, encoded_col: u32, encoding: PosEncoding) -> usize {
    let encoded_col = encoded_col as usize;
    match encoding {
        PosEncoding::Utf8 => encoded_col.min(line.len()),
        _ => {
            if line.is_ascii() {
                return encoded_col.min(line.len());
            }
            match encoding {
                PosEncoding::Utf32 => {
                    line.char_indices()
                        .nth(encoded_col)
                        .map(|(i, _)| i)
                        .unwrap_or(line.len())
                }
                PosEncoding::Utf16 => {
                    let mut utf16_offset = 0usize;
                    for (i, ch) in line.char_indices() {
                        if utf16_offset >= encoded_col {
                            return i;
                        }
                        utf16_offset += ch.len_utf16();
                    }
                    line.len()
                }
                PosEncoding::Utf8 => unreachable!(),
            }
        }
    }
}

pub struct QuickLspServer {
    client: Client,
    workspace: Arc<Workspace>,
    dep_index: Arc<DependencyIndex>,
    workspace_root: Arc<RwLock<Option<PathBuf>>>,
    pos_encoding: RwLock<PosEncoding>,
}

impl QuickLspServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            workspace: Arc::new(Workspace::new()),
            dep_index: Arc::new(DependencyIndex::new()),
            workspace_root: Arc::new(RwLock::new(None)),
            pos_encoding: RwLock::new(PosEncoding::Utf16),
        }
    }

    /// Convert an incoming client position to a char index for the given line.
    fn client_col_to_char_index(
        line: &str,
        encoded_col: u32,
        encoding: PosEncoding,
    ) -> usize {
        let byte_offset = encoding_col_to_byte(line, encoded_col, encoding);
        // Convert byte offset to char index
        line[..byte_offset.min(line.len())].chars().count()
    }

    fn word_at_position(content: &str, line_idx: usize, col: usize) -> Option<String> {
        let line = content.lines().nth(line_idx)?;
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
    fn qualifier_at_position(content: &str, line_idx: usize, col: usize) -> Option<String> {
        let line = content.lines().nth(line_idx)?;
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

/// Get the source line for a symbol location from the workspace.
fn get_source_line(ws: &Workspace, loc: &SymbolLocation) -> Option<String> {
    let source = ws.file_source(&loc.file)?;
    source.lines().nth(loc.symbol.line).map(|s| s.to_string())
}

fn loc_to_lsp(loc: &SymbolLocation, source_line: Option<&str>, enc: PosEncoding) -> Option<Location> {
    let uri = Url::from_file_path(&loc.file).ok()?;
    let (start_char, end_char) = match source_line {
        Some(line) => (
            byte_col_to_encoding(line, loc.symbol.col, enc),
            byte_col_to_encoding(line, loc.symbol.col + loc.symbol.name.len(), enc),
        ),
        None => (loc.symbol.col as u32, (loc.symbol.col + loc.symbol.name.len()) as u32),
    };
    Some(Location {
        uri,
        range: Range {
            start: Position {
                line: loc.symbol.line as u32,
                character: start_char,
            },
            end: Position {
                line: loc.symbol.line as u32,
                character: end_char,
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

        // Negotiate position encoding: prefer UTF-8, then UTF-32, fall back to UTF-16
        let client_encodings = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_ref());

        let (negotiated, encoding_kind) = if let Some(encodings) = client_encodings {
            if encodings.iter().any(|e| *e == PositionEncodingKind::UTF8) {
                (PosEncoding::Utf8, Some(PositionEncodingKind::UTF8))
            } else if encodings.iter().any(|e| *e == PositionEncodingKind::UTF32) {
                (PosEncoding::Utf32, Some(PositionEncodingKind::UTF32))
            } else {
                (PosEncoding::Utf16, None)
            }
        } else {
            (PosEncoding::Utf16, None)
        };
        *self.pos_encoding.write().await = negotiated;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: encoding_kind,
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

            let ws_defs = workspace.definition_count();
            let ws_files = workspace.file_count();
            let dep_pkgs = dep_index.package_count();
            let dep_defs = dep_index.definition_count();
            let done_msg = format!(
                "Indexed {ws_files} files ({ws_defs} definitions), {dep_pkgs} dependency packages ({dep_defs} definitions)",
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
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_str = source.lines().nth(pos.line as usize);
        let char_col = line_str
            .map(|l| Self::client_col_to_char_index(l, pos.character, enc))
            .unwrap_or(0);
        let symbol = match Self::word_at_position(&source, pos.line as usize, char_col) {
            Some(s) => s,
            None => return Ok(None),
        };
        let qualifier = Self::qualifier_at_position(&source, pos.line as usize, char_col);
        let current_file = uri.to_file_path().ok();
        let mut defs = self.workspace.find_definitions(&symbol);
        if defs.is_empty() {
            defs = self.dep_index.find_definitions(&symbol);
        }
        self.workspace
            .rank_definitions(&mut defs, current_file.as_deref(), qualifier.as_deref());
        if let Some(def) = defs.first() {
            let src_line = get_source_line(&self.workspace, def);
            if let Some(loc) = loc_to_lsp(def, src_line.as_deref(), enc) {
                return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
            }
        }
        Ok(None)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_str = source.lines().nth(pos.line as usize);
        let char_col = line_str
            .map(|l| Self::client_col_to_char_index(l, pos.character, enc))
            .unwrap_or(0);
        let symbol = match Self::word_at_position(&source, pos.line as usize, char_col) {
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
                let ref_line = self.workspace.file_source(&r.file).and_then(|s| {
                    s.lines().nth(r.line).map(|l| l.to_string())
                });
                let ref_line_str = ref_line.as_deref().unwrap_or("");
                Some(Location {
                    uri: u,
                    range: Range {
                        start: Position {
                            line: r.line as u32,
                            character: byte_col_to_encoding(ref_line_str, r.col, enc),
                        },
                        end: Position {
                            line: r.line as u32,
                            character: byte_col_to_encoding(ref_line_str, r.col + r.len, enc),
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
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document.uri;
        let path = match uri.to_file_path() {
            Ok(p) => p,
            Err(_) => return Ok(None),
        };
        let syms = self.workspace.file_symbols(&path);
        if syms.is_empty() {
            return Ok(None);
        }
        let source = self.workspace.file_source(&path);
        let lines: Vec<&str> = source.as_deref().map(|s| s.lines().collect()).unwrap_or_default();
        let lsp_syms: Vec<SymbolInformation> = syms
            .iter()
            .map(|s| {
                let line_str = lines.get(s.line).copied().unwrap_or("");
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
                                character: byte_col_to_encoding(line_str, s.col, enc),
                            },
                            end: Position {
                                line: s.line as u32,
                                character: byte_col_to_encoding(line_str, s.col + s.name.len(), enc),
                            },
                        },
                    },
                    container_name: s.container.clone(),
                }
            })
            .collect();
        Ok(Some(DocumentSymbolResponse::Flat(lsp_syms)))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        use crate::parsing::tokenizer::Visibility;
        let enc = *self.pos_encoding.read().await;
        if params.query.is_empty() {
            return Ok(None);
        }
        let mut results = self.workspace.search_symbols(&params.query);
        if results.is_empty() {
            return Ok(None);
        }
        // Rank public symbols higher than private ones
        results.sort_by(|a, b| {
            let vis_score = |v: Visibility| match v {
                Visibility::Public => 2,
                Visibility::Unknown => 1,
                Visibility::Private => 0,
            };
            vis_score(b.symbol.visibility).cmp(&vis_score(a.symbol.visibility))
        });
        let syms: Vec<SymbolInformation> = results
            .iter()
            .take(20)
            .filter_map(|loc| {
                let src_line = get_source_line(&self.workspace, loc);
                let location = loc_to_lsp(loc, src_line.as_deref(), enc)?;
                #[allow(deprecated)]
                Some(SymbolInformation {
                    name: loc.symbol.name.clone(),
                    kind: aero_kind_to_lsp(loc.symbol.kind),
                    tags: None,
                    deprecated: None,
                    location,
                    container_name: loc.symbol.container.clone(),
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
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_str = source.lines().nth(pos.line as usize);
        let char_col = line_str
            .map(|l| Self::client_col_to_char_index(l, pos.character, enc))
            .unwrap_or(0);
        let symbol = match Self::word_at_position(&source, pos.line as usize, char_col) {
            Some(s) => s,
            None => return Ok(None),
        };

        // Find definitions and rank them by context (qualifier + same-file)
        let qualifier = Self::qualifier_at_position(&source, pos.line as usize, char_col);
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
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };

        // signature_help_at works on char indices internally
        let line_str = source.lines().nth(pos.line as usize);
        let char_col = line_str
            .map(|l| Self::client_col_to_char_index(l, pos.character, enc))
            .unwrap_or(0);

        let (loc, active_param) = match self.workspace.signature_help_at(
            &source,
            pos.line as usize,
            char_col,
        ) {
            Some(result) => result,
            // Fallback to dependency index
            None => match self.dep_index.signature_help_at(
                &source,
                pos.line as usize,
                char_col,
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
        let enc = *self.pos_encoding.read().await;
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let source = match self.workspace.file_source_from_uri(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let line_str = source.lines().nth(pos.line as usize);
        let char_col = line_str
            .map(|l| Self::client_col_to_char_index(l, pos.character, enc))
            .unwrap_or(0);
        let partial = match Self::word_at_position(&source, pos.line as usize, char_col) {
            Some(s) if !s.is_empty() => s,
            _ => return Ok(None),
        };
        let current_file = uri.to_file_path().ok();
        let mut results = self.workspace.completions(&partial);
        // Merge dependency completions
        let dep_results = self.dep_index.completions(&partial);
        results.extend(dep_results);
        if results.is_empty() {
            return Ok(None);
        }
        // Rank: same-file private > public > unknown > other-file private
        {
            use crate::parsing::tokenizer::Visibility;
            results.sort_by(|a, b| {
                let vis_score = |loc: &crate::workspace::SymbolLocation| {
                    let same_file = current_file.as_deref() == Some(loc.file.as_path());
                    match (loc.symbol.visibility, same_file) {
                        (Visibility::Private, true) => 3, // same-file private is best
                        (Visibility::Public, _) => 2,
                        (Visibility::Unknown, _) => 1,
                        (Visibility::Private, false) => 0,
                    }
                };
                vis_score(b).cmp(&vis_score(a))
            });
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
