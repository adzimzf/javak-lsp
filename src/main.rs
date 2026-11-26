use std::sync::Arc;

use javak_lsp::{backend::service::service, indexer::Indexer};
use tower_lsp::Server;

#[tokio::main]
async fn main() {
    let indexer = Arc::new(Indexer::new());
    let (service, socket) = service(indexer);
    // Library searches wait their turn without taking the place of the
    // quick requests behind them: up to 64 requests are in hand at once.
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .concurrency_level(64)
        .serve(service)
        .await;
}
