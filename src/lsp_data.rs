// Copyright 2016 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Types, helpers, and conversions to and from LSP and `racer` types.

use std::collections::HashMap;
use std::fmt::{self, Debug};
use std::path::PathBuf;
use std::error::Error;

use analysis::DefKind;
use url::Url;
use serde::Serialize;
use span;
use racer;
use vfs::FileContents;

pub use ls_types::*;
use jsonrpc_core::version;

/// Notification string for beginning diagnostics.
pub const NOTIFICATION_DIAGNOSTICS_BEGIN: &'static str = "rustDocument/diagnosticsBegin";
/// Notification string for ending diagnostics.
pub const NOTIFICATION_DIAGNOSTICS_END:   &'static str = "rustDocument/diagnosticsEnd";
/// Notification string for when a build begins.
pub const NOTIFICATION_BUILD_BEGIN:       &'static str = "rustDocument/beginBuild";

/// Errors that can occur when parsing a file URI.
#[derive(Debug)]
pub enum UrlFileParseError {
    /// The URI scheme is not `file`.
    InvalidScheme,
    /// Invalid file path in the URI.
    InvalidFilePath
}

impl Error for UrlFileParseError {
    fn description(&self) -> &str {
        match *self {
            UrlFileParseError::InvalidScheme => "URI scheme is not `file`",
            UrlFileParseError::InvalidFilePath => "Invalid file path in URI",
        }
    }
}

impl fmt::Display for UrlFileParseError where UrlFileParseError: Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

/// Parse the given URI into a `PathBuf`.
pub fn parse_file_path(uri: &Url) -> Result<PathBuf, UrlFileParseError> {
    if uri.scheme() != "file" {
        Err(UrlFileParseError::InvalidScheme)
    } else {
        uri.to_file_path().map_err(|_err| UrlFileParseError::InvalidFilePath)
    }
}

/// Create an edit for the given location and text.
pub fn make_workspace_edit(location: Location, new_text: String) -> WorkspaceEdit {
    let mut edit = WorkspaceEdit {
        changes: HashMap::new(),
    };

    edit.changes.insert(location.uri, vec![TextEdit {
        range: location.range,
        new_text,
    }]);

    edit
}

/// Utilities for working with the language server protocol.
pub mod ls_util {
    use super::*;
    use Span;

    use std::path::Path;
    use vfs::Vfs;

    /// Convert a language server protocol range into an RLS range.
    pub fn range_to_rls(r: Range) -> span::Range<span::ZeroIndexed> {
        span::Range::from_positions(position_to_rls(r.start), position_to_rls(r.end))
    }

    /// Convert a language server protocol position into an RLS position.
    pub fn position_to_rls(p: Position) -> span::Position<span::ZeroIndexed> {
        span::Position::new(span::Row::new_zero_indexed(p.line as u32),
                            span::Column::new_zero_indexed(p.character as u32))
    }

    /// Convert a language server protocol location into an RLS span.
    pub fn location_to_rls(l: Location) -> Result<span::Span<span::ZeroIndexed>, UrlFileParseError> {
        parse_file_path(&l.uri).map(|path| Span::from_range(range_to_rls(l.range), path))
    }

    /// Convert an RLS span into a language server protocol location.
    pub fn rls_to_location(span: &Span) -> Location {
        // An RLS span has the same info as an LSP Location
        Location {
            uri: Url::from_file_path(&span.file).unwrap(),
            range: rls_to_range(span.range),
        }
    }

    /// Convert an RLS location into a language server protocol location.
    pub fn rls_location_to_location(l: &span::Location<span::ZeroIndexed>) -> Location {
        Location {
            uri: Url::from_file_path(&l.file).unwrap(),
            range: rls_to_range(span::Range::from_positions(l.position, l.position)),
        }
    }

    /// Convert an RLS range into a language server protocol range.
    pub fn rls_to_range(r: span::Range<span::ZeroIndexed>) -> Range {
        Range {
            start: rls_to_position(r.start()),
            end: rls_to_position(r.end()),
        }
    }

    /// Convert an RLS position into a language server protocol range.
    pub fn rls_to_position(p: span::Position<span::ZeroIndexed>) -> Position {
        Position {
            line: p.row.0 as u64,
            character: p.col.0 as u64,
        }
    }

    /// Creates a `Range` spanning the whole file as currently known by `Vfs`
    ///
    /// Panics if `Vfs` cannot load the file.
    pub fn range_from_vfs_file(vfs: &Vfs, fname: &Path) -> Range {
        // FIXME load_file clones the entire file text, this could be much more
        // efficient by adding a `with_file` fn to the VFS.
        let content = match vfs.load_file(fname).unwrap() {
            FileContents::Text(t) => t,
            _ => panic!("unexpected binary file: {:?}", fname),
        };
        if content.is_empty() {
            Range {start: Position::new(0, 0), end: Position::new(0, 0)}
        } else {
            let mut line_count = content.lines().count() as u64 - 1;
            let col = if content.ends_with('\n') {
                line_count += 1;
                0
            } else {
                content.lines().last().expect("String is not empty.").chars().count() as u64
            };
            // range is zero-based and the end position is exclusive
            Range {
                start: Position::new(0, 0),
                end: Position::new(line_count, col),
            }
        }
    }
}

/// Convert an RLS def-kind to a language server protocol symbol-kind.
pub fn source_kind_from_def_kind(k: DefKind) -> SymbolKind {
    match k {
        DefKind::Enum => SymbolKind::Enum,
        DefKind::TupleVariant => SymbolKind::Constant,
        DefKind::StructVariant => SymbolKind::Class,
        DefKind::Tuple => SymbolKind::Array,
        DefKind::Struct => SymbolKind::Class,
        DefKind::Union => SymbolKind::Class,
        DefKind::Trait => SymbolKind::Interface,
        DefKind::Function |
        DefKind::Method |
        DefKind::Macro => SymbolKind::Function,
        DefKind::Mod => SymbolKind::Module,
        DefKind::Type |
        DefKind::ExternType => SymbolKind::Interface,
        DefKind::Local |
        DefKind::Static |
        DefKind::Const |
        DefKind::Field => SymbolKind::Variable,
    }
}

/// What kind of completion is this racer match type?
pub fn completion_kind_from_match_type(m : racer::MatchType) -> CompletionItemKind {
    match m {
        racer::MatchType::Crate |
        racer::MatchType::Module => CompletionItemKind::Module,
        racer::MatchType::Struct => CompletionItemKind::Class,
        racer::MatchType::Enum => CompletionItemKind::Enum,
        racer::MatchType::StructField |
        racer::MatchType::EnumVariant => CompletionItemKind::Field,
        racer::MatchType::Macro |
        racer::MatchType::Function |
        racer::MatchType::FnArg |
        racer::MatchType::Impl => CompletionItemKind::Function,
        racer::MatchType::Type |
        racer::MatchType::Trait |
        racer::MatchType::TraitImpl => CompletionItemKind::Interface,
        racer::MatchType::Let |
        racer::MatchType::IfLet |
        racer::MatchType::WhileLet |
        racer::MatchType::For |
        racer::MatchType::MatchArm |
        racer::MatchType::Const |
        racer::MatchType::Static => CompletionItemKind::Variable,
        racer::MatchType::Builtin => CompletionItemKind::Keyword,
    }
}

/// Convert a racer match into an RLS completion.
pub fn completion_item_from_racer_match(m : racer::Match) -> CompletionItem {
    let mut item = CompletionItem::new_simple(m.matchstr.clone(), m.contextstr.clone());
    item.kind = Some(completion_kind_from_match_type(m.mtype));

    item
}

/* -----------------  JSON-RPC protocol types ----------------- */

/// Supported initilization options that can be passed in the `initialize`
/// request, under `initialization_options` key. These are specific to the RLS.
#[derive(Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct InitializationOptions {
    /// Should the build not be triggered immediately after receiving `initialize`
    #[serde(rename="omitInitBuild")]
    pub omit_init_build: bool,
}

impl Default for InitializationOptions {
    fn default() -> Self {
        InitializationOptions {
            omit_init_build: false
        }
    }
}

/// An event-like (no response needed) notification message.
#[derive(Debug, Serialize)]
pub struct NotificationMessage {
    jsonrpc: version::Version,
    /// The well-known language server protocol notification method string.
    pub method: &'static str,
    /// Extra notification parameters.
    pub params: Option<PublishDiagnosticsParams>,
}

impl NotificationMessage {
    /// Construct a new notification message.
    pub fn new(method: &'static str, params: Option<PublishDiagnosticsParams>) -> Self {
        NotificationMessage {
            jsonrpc: version::Version::V2,
            method,
            params,
        }
    }
}

/// A JSON language server protocol request that will have a matching response.
#[derive(Debug, Serialize)]
pub struct RequestMessage<T>
    where T: Debug + Serialize
{
    jsonrpc: &'static str,
    /// The request id. The response will have a matching id.
    pub id: u32,
    /// The well-known language server protocol request method string.
    pub method: String,
    /// Extra request parameters.
    pub params: T,
}

impl <T> RequestMessage<T> where T: Debug + Serialize {
    /// Construct a new request.
    pub fn new(id: u32, method: String, params: T) -> Self {
        RequestMessage {
            jsonrpc: "2.0",
            id,
            method: method,
            params: params
        }
    }
}
