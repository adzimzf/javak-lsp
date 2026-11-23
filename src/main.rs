use std::sync::Arc;

use javak_lsp::{backend_over, indexer::Indexer};
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let indexer = Arc::new(Indexer::new());
    let (service, socket) = LspService::new(move |client| backend_over(indexer.clone(), client));
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}
