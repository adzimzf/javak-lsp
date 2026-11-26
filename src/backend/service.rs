//! The server as a service: tower-lsp's, with the capabilities added to
//! the `initialize` answer that lsp-types 0.94 has no field for (type
//! hierarchy, LSP 3.17), though tower-lsp routes their requests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use serde_json::Value;
use tower_lsp::jsonrpc::{Request, Response};
use tower_lsp::{ClientSocket, ExitedError, LspService};
use tower_service::Service;

use super::{backend_over, Backend};
use crate::indexer::Indexer;

/// The capabilities `initialize` adds to what tower-lsp writes.
const ADDED_CAPABILITIES: &[&str] = &["typeHierarchyProvider"];

/// The service and its socket, over `indexer`.
pub fn service(indexer: Arc<Indexer>) -> (Advertised, ClientSocket) {
    let (inner, socket) = LspService::new(move |client| backend_over(indexer, client));
    (Advertised { inner }, socket)
}

/// tower-lsp's service, its `initialize` answer naming what was added.
pub struct Advertised {
    inner: LspService<Backend>,
}

impl Service<Request> for Advertised {
    type Response = Option<Response>;
    type Error = ExitedError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let initializing = request.method() == "initialize";
        let answer = self.inner.call(request);
        Box::pin(async move {
            let response = answer.await?;
            if !initializing {
                return Ok(response);
            }
            Ok(response.map(|response| {
                let (id, body) = response.into_parts();
                let body = body.map(|mut result| {
                    if let Some(capabilities) = result.get_mut("capabilities").and_then(Value::as_object_mut) {
                        for added in ADDED_CAPABILITIES {
                            capabilities.insert((*added).to_owned(), Value::Bool(true));
                        }
                    }
                    result
                });
                Response::from_parts(id, body)
            }))
        })
    }
}
