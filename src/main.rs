use std::sync::Arc;

use javak_lsp::{backend::service::service, indexer::Indexer};
use tower_lsp::Server;

#[tokio::main]
async fn main() {
    let indexer = Arc::new(Indexer::new());
    let (service, socket) = service(indexer);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
}
