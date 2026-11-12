//! detag-worker — Apalis worker host (bootstrap stub, P0-002).
//! Queue wiring lands in E0.8 (P0-075..P0-082).

#[tokio::main]
async fn main() {
    println!("detag-worker started (queues: pipeline, cleanup, email, reconciliation)");
    tokio::signal::ctrl_c().await.unwrap();
    println!("detag-worker shutting down");
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_crate_loads() {
        assert!(!env!("CARGO_PKG_NAME").is_empty());
    }
}
