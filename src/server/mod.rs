// Copyright 2017 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Implementation of the server loop, and traits for extending server
//! interactions (for example, to add support for handling new types of
//! requests).

use analysis::AnalysisHost;
use jsonrpc_core::{self as jsonrpc, Id};
use vfs::Vfs;
use serde;
use serde_json;
use serde::Deserialize;

use version;
use lsp_data::*;
use actions::{ActionContext, requests, notifications};
use config::Config;
pub use server::io::{MessageReader, Output};
use server::io::{StdioMsgReader, StdioOutput};

use std::fmt;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

mod io;

/// Run the Rust Language Server.
pub fn run_server(analysis: Arc<AnalysisHost>, vfs: Arc<Vfs>) {
    debug!("Language Server starting up. Version: {}", version());
    let service = LsService::new(analysis,
                                 vfs,
                                 Arc::new(Mutex::new(Config::default())),
                                 Box::new(StdioMsgReader),
                                 StdioOutput::new());
    LsService::run(service);
    debug!("Server shutting down");
}

/// A response that just acknowledges receipt of its request.
#[derive(Debug, Serialize)]
pub struct Ack;

/// The lack of a response to a request.
#[derive(Debug)]
pub struct NoResponse;

/// Empty extra parameters to some request or notification.
#[derive(Debug, Serialize, PartialEq)]
pub struct NoParams;

impl<'de> Deserialize<'de> for NoParams {
    fn deserialize<D>(_deserializer: D) -> Result<NoParams, D::Error>
        where D: serde::Deserializer<'de>,
    {
        Ok(NoParams)
    }
}

/// A response to some request.
pub trait Response {
    /// Send the response along the given output.
    fn send<O: Output>(&self, id: usize, out: O);
}

impl Response for NoResponse {
    fn send<O: Output>(&self, _id: usize, _out: O) {
    }
}

impl<R: ::serde::Serialize + fmt::Debug> Response for R {
    fn send<O: Output>(&self, id: usize, out: O) {
        out.success(id, &self);
    }
}

/// An action taken by the Rust Language Server.
pub trait Action<'a> {
    /// Extra parameters that the action expects to receive.
    type Params: serde::Serialize + for<'de> ::serde::Deserialize<'de>;

    /// The well-known language server method string that identifies this
    /// action's kind of request or notification.
    const METHOD: &'static str;

    /// Construct a new instance of this action from the given language server
    /// state.
    fn new(state: &'a mut LsState) -> Self;
}

/// An action taken in response to some notification from the client.
pub trait NotificationAction<'a>: Action<'a> {
    /// Handle this notification.
    fn handle<O: Output>(&mut self, params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<(), ()>;
}

/// An action that implements support for handling requests from the client and
/// replying with a corresponding response.
pub trait RequestAction<'a>: Action<'a> {
    /// The kind of response for this request.
    type Response: Response + fmt::Debug;

    /// Handle request and send its response back along the given output.
    fn handle<O: Output>(&mut self, id: usize, params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<Self::Response, ()>;
}

/// A request that gets JSON serialized in the language server protocol.
pub struct Request<'a, A: RequestAction<'a>> {
    /// The unique request id.
    pub id: usize,
    /// The extra action-specific parameters.
    pub params: A::Params,
    /// This request's handler action.
    pub _action: PhantomData<A>,
}

/// A notification that gets JSON serialized in the language server protocol.
#[derive(Debug, PartialEq)]
pub struct Notification<'a, A: NotificationAction<'a>> {
    /// The extra action-specific parameters.
    pub params: A::Params,
    /// The action responsible for this notification.
    pub _action: PhantomData<A>,
}

impl<'a, A: RequestAction<'a>> Request<'a, A> {
    fn dispatch<O: Output>(self, state: &'a mut LsState, ctx: &mut ActionContext, out: O) -> Result<A::Response, ()> {
        let mut action = A::new(state);
        let result = action.handle(self.id, self.params, ctx, out.clone())?;
        result.send(self.id, out);
        Ok(result)
    }
}

impl<'a, A: NotificationAction<'a>> Notification<'a, A> {
    fn dispatch<O: Output>(self, state: &'a mut LsState, ctx: &mut ActionContext, out: O) -> Result<(), ()> {
        let mut action = A::new(state);
        action.handle(self.params, ctx, out)?;
        Ok(())
    }
}

impl<'a, A: RequestAction<'a>> fmt::Display for Request<'a, A> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "method": A::METHOD,
            "params": self.params,
        }).fmt(f)
    }
}

impl<'a, A: NotificationAction<'a>> fmt::Display for Notification<'a, A> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        json!({
            "jsonrpc": "2.0",
            "method": A::METHOD,
            "params": self.params,
        }).fmt(f)
    }
}

/// A service implementing a language server.
pub struct LsService<O: Output> {
    msg_reader: Box<MessageReader + Send + Sync>,
    output: O,
    ctx: ActionContext,
    /// The public shared state for this language server.
    pub state: LsState,
}

/// Public shared state for this language server.
#[derive(Debug)]
pub struct LsState {
    shut_down: AtomicBool,
}

/// A request to shutdown the language server and perform clean up, but not to
/// exit the process. After receiving a response to this request, the client
/// will send an `exit` notification, at which point we will actually exit the
/// process.
pub struct ShutdownRequest<'a> {
    state: &'a mut LsState,
}

impl<'a> Action<'a> for ShutdownRequest<'a> {
    type Params = NoParams;
    const METHOD: &'static str = "shutdown";

    fn new(state: &'a mut LsState) -> Self {
        ShutdownRequest {
            state
        }
    }
}

impl<'a> RequestAction<'a> for ShutdownRequest<'a> {
    type Response = Ack;
    fn handle<O: Output>(&mut self, _id: usize, _params: Self::Params, _ctx: &mut ActionContext, _out: O) -> Result<Self::Response, ()> {
        self.state.shut_down.store(true, Ordering::SeqCst);
        Ok(Ack)
    }
}

/// Notification that it is time for the language server to exit its process.
#[derive(Debug)]
pub struct ExitNotification<'a> {
    state: &'a mut LsState,
}

impl<'a> Action<'a> for ExitNotification<'a> {
    type Params = NoParams;
    const METHOD: &'static str = "exit";

    fn new(state: &'a mut LsState) -> Self {
        ExitNotification {
            state
        }
    }
}

impl<'a> NotificationAction<'a> for ExitNotification<'a> {
    fn handle<O: Output>(&mut self, _params: Self::Params, _ctx: &mut ActionContext, _out: O) -> Result<(), ()> {
        let shut_down = self.state.shut_down.load(Ordering::SeqCst);
        ::std::process::exit(if shut_down { 0 } else { 1 });
    }
}

/// A request to initialize this server.
pub struct InitializeRequest;

impl<'a> Action<'a> for InitializeRequest {
    type Params = InitializeParams;
    const METHOD: &'static str = "initialize";

    fn new(_: &'a mut LsState) -> Self {
        InitializeRequest
    }
}

fn get_root_path(params: &InitializeParams) -> PathBuf {
    params.root_uri.as_ref().map(|uri| {
        assert!(uri.scheme() == "file");
        uri.to_file_path().expect("Could not convert URI to path")
    }).unwrap_or_else(|| {
        params.root_path.as_ref().map(PathBuf::from).expect("No root path or URI")
    })
}

impl<'a> RequestAction<'a> for InitializeRequest {
    type Response = NoResponse;
    fn handle<O: Output>(&mut self, id: usize, params: Self::Params, ctx: &mut ActionContext, out: O) -> Result<NoResponse, ()> {
        let init_options: InitializationOptions = params
            .initialization_options
            .as_ref()
            .and_then(|options| serde_json::from_value(options.to_owned()).ok())
            .unwrap_or_default();

        trace!("init: {:?}", init_options);

        let result = InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncKind::Incremental),
                hover_provider: Some(true),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(true),
                    trigger_characters: vec![".".to_string(), ":".to_string()],
                }),
                definition_provider: Some(true),
                references_provider: Some(true),
                document_highlight_provider: Some(true),
                document_symbol_provider: Some(true),
                workspace_symbol_provider: Some(true),
                code_action_provider: Some(true),
                document_formatting_provider: Some(true),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["rls.applySuggestion".to_owned()],
                }),
                rename_provider: Some(true),
                // These are supported if the `unstable_features` option is set.
                // We'll update these capabilities dynamically when we get config
                // info from the client.
                document_range_formatting_provider: Some(false),

                code_lens_provider: None,
                document_on_type_formatting_provider: None,
                signature_help_provider: None,
            }
        };
        out.success(id, &result);

        ctx.init(get_root_path(&params), &init_options, out);

        Ok(NoResponse)
    }
}

/// How should the server proceed?
#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum ServerStateChange {
    /// Continue serving responses to requests and sending notifications to the
    /// client.
    Continue,
    /// Stop the server.
    Break,
}

impl<O: Output> LsService<O> {
    /// Construct a new language server service.
    pub fn new(analysis: Arc<AnalysisHost>,
               vfs: Arc<Vfs>,
               config: Arc<Mutex<Config>>,
               reader: Box<MessageReader + Send + Sync>,
               output: O)
               -> LsService<O> {
        LsService {
            msg_reader: reader,
            output: output,
            ctx: ActionContext::new(analysis, vfs, config),
            state: LsState {
                shut_down: AtomicBool::new(false),
            }
        }
    }

    /// Run this language service.
    pub fn run(mut self) {
        while self.handle_message() == ServerStateChange::Continue {}
    }

    fn parse_message(&mut self, msg: &str) -> Result<Option<RawMessage>, jsonrpc::Error> {
        // Parse the message.
        let ls_command: serde_json::Value = serde_json::from_str(msg).map_err(|_| jsonrpc::Error::parse_error())?;

        // Per JSON-RPC/LSP spec, Requests must have id, whereas Notifications can't
        let id = ls_command.get("id").map(|id| serde_json::from_value(id.to_owned()).unwrap());

        let method = match ls_command.get("method") {
            Some(method) => method,
            // No method means this is a response to one of our requests. FIXME: we should
            // confirm these, but currently just ignore them.
            None => return Ok(None),
        };

        let method = method.as_str().ok_or_else(|| jsonrpc::Error::invalid_request())?.to_owned();

        // Representing internally a missing parameter as Null instead of None,
        // (Null being unused value of param by the JSON-RPC 2.0 spec)
        // to unify the type handling – now the parameter type implements Deserialize.
        let params = match ls_command.get("params").map(|p| p.to_owned()) {
            Some(params @ serde_json::Value::Object(..)) => params,
            Some(params @ serde_json::Value::Array(..)) => params,
            None => serde_json::Value::Null,
            // Null as input value is not allowed by JSON-RPC 2.0,
            //but including it for robustness
            Some(serde_json::Value::Null) => serde_json::Value::Null,
            _ => return Err(jsonrpc::Error::invalid_request()),
        };

        Ok(Some(RawMessage { method, id, params }))
    }

    fn dispatch_message(&mut self, msg: &RawMessage) -> Result<(), jsonrpc::Error> {
        macro_rules! match_action {
            ($method: expr; notifications: $($n_action: ty),*; requests: $($r_action: ty),*;) => {
                let mut handled = false;
                trace!("Handling `{}`", $method);
                $(
                    if $method == <$n_action as Action>::METHOD {
                        let notification = msg.parse_as_notification::<$n_action>()?;
                        if let Err(_) = notification.dispatch(&mut self.state, &mut self.ctx, self.output.clone()) {
                            debug!("Error handling notification: {:?}", msg);
                        }
                        handled = true;
                    }
                )*
                $(
                    if $method == <$r_action as Action>::METHOD {
                        let request = msg.parse_as_request::<$r_action>()?;
                        if let Err(_) = request.dispatch(&mut self.state, &mut self.ctx, self.output.clone()) {
                            debug!("Error handling notification: {:?}", msg);
                        }
                        handled = true;
                    }
                )*
                if !handled {
                    debug!("Method not found: {}", $method);
                }
            }
        }

        match_action!(
            msg.method;
            notifications:
                ExitNotification,
                notifications::Initialized,
                notifications::DidOpen,
                notifications::DidChange,
                notifications::DidSave,
                notifications::DidChangeConfiguration,
                notifications::DidChangeWatchedFiles,
                notifications::Cancel;
            requests:
                ShutdownRequest,
                InitializeRequest,
                requests::Definition,
                requests::References,
                requests::Completion,
                requests::ResolveCompletion,
                requests::Rename,
                requests::DocumentHighlight,
                requests::ExecuteCommand,
                requests::CodeAction,
                requests::FindImpls,
                requests::Deglob,
                requests::Symbols,
                requests::WorkspaceSymbol,
                requests::Formatting,
                requests::RangeFormatting,
                requests::Hover;
        );
        Ok(())
    }

    /// Read a message from the language server reader input and handle it with
    /// the appropriate action. Returns a `ServerStateChange` that describes how
    /// the service should proceed now that the message has been handled.
    pub fn handle_message(&mut self) -> ServerStateChange {
        let msg_string = match self.msg_reader.read_message() {
            Some(m) => m,
            None => {
                debug!("Can't read message");
                self.output.failure(Id::Null, jsonrpc::Error::parse_error());
                return ServerStateChange::Break;
            },
        };

        trace!("Read message `{}`", msg_string);

        let raw_message = match self.parse_message(&msg_string) {
            Ok(Some(rm)) => rm,
            Ok(None) => return ServerStateChange::Continue,
            Err(e) => {
                debug!("parsing error, {:?}", e);
                self.output.failure(Id::Null, jsonrpc::Error::parse_error());
                return ServerStateChange::Break;
            }
        };

        trace!("Parsed message `{:?}`", raw_message);

        // If we're in shutdown mode, ignore any messages other than 'exit'.
        // This is not actually in the spec, I'm not sure we should do this,
        // but it kinda makes sense.
        {
            let shut_down = self.state.shut_down.load(Ordering::SeqCst);
            if shut_down && raw_message.method != ExitNotification::METHOD {
                trace!("In shutdown mode, ignoring {:?}!", raw_message);
                return ServerStateChange::Continue;
            }
        }

        if let Err(e) = self.dispatch_message(&raw_message) {
            debug!("dispatch error, {:?}", e);
            self.output.failure(raw_message.id.unwrap_or(Id::Null), e);
            return ServerStateChange::Break;
        }

        ServerStateChange::Continue
    }
}

#[derive(Debug)]
struct RawMessage {
    method: String,
    id: Option<Id>,
    params: serde_json::Value,
}

impl RawMessage {
    fn parse_as_request<'a, T: RequestAction<'a>>(&'a self) -> Result<Request<T>, jsonrpc::Error> {

        // FIXME: We only support numeric responses, ideally we should switch from using parsed usize
        // to using jsonrpc_core::Id
        let parsed_numeric_id = match &self.id {
            &Some(Id::Num(n)) => Some(n as usize),
            &Some(Id::Str(ref s)) => usize::from_str_radix(s, 10).ok(),
            _ => None,
        };

        let params = T::Params::deserialize(&self.params)
            .map_err(|e| {
                debug!("error when parsing as request: {}", e);
                jsonrpc::Error::invalid_request()
            })?;

        match parsed_numeric_id {
            Some(id) => {
                Ok(Request {
                    id,
                    params,
                    _action: PhantomData,
                })
            }
            None => return Err(jsonrpc::Error::invalid_request()),
        }
    }

    fn parse_as_notification<'a, T: NotificationAction<'a>>(&'a self) -> Result<Notification<T>, jsonrpc::Error> {
        use serde::Deserialize;

        let params = T::Params::deserialize(&self.params)
            .map_err(|e| {
                debug!("error when parsing as notification: {}", e);
                jsonrpc::Error::invalid_request()
            })?;

        Ok(Notification {
            params,
            _action: PhantomData,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use url::Url;

    fn get_default_params() -> InitializeParams {
        InitializeParams {
            process_id: None,
            root_path: None,
            root_uri: None,
            initialization_options: None,
            capabilities: ClientCapabilities {
                workspace: None,
                text_document: None,
                experimental: None,
            },
            trace: TraceOption::Off,
        }
    }

    fn make_platform_path(path: &'static str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:/{}", path))
        } else {
            PathBuf::from(format!("/{}", path))
        }
    }

    #[test]
    fn test_use_root_uri() {
        let mut params = get_default_params();

        let root_path = make_platform_path("path/a");
        let root_uri = make_platform_path("path/b");
        params.root_path = Some(root_path.to_str().unwrap().to_owned());
        params.root_uri = Some(Url::from_directory_path(&root_uri).unwrap());

        assert_eq!(get_root_path(&params), root_uri);
    }

    #[test]
    fn test_use_root_path() {
        let mut params = get_default_params();

        let root_path = make_platform_path("path/a");
        params.root_path = Some(root_path.to_str().unwrap().to_owned());
        params.root_uri = None;

        assert_eq!(get_root_path(&params), root_path);
    }

    #[test]
    fn test_parse_as_notification() {
        let raw = RawMessage {
            method: "initialize".to_owned(),
            id: None,
            params: serde_json::Value::Null,
        };
        let notification = raw.parse_as_notification::<notifications::Initialized>();

        assert_eq!(notification, Ok(Notification::<notifications::Initialized> {
            params: NoParams {},
            _action: PhantomData,
        }));
    }
}
