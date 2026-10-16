//! AgentIM Lite: the communication and record layer for agents built with any framework (docs/01-product.md §6).

#[tokio::main]
async fn main() {
    agentim_core::cli::run_with(
        agentim_core::edition::Edition::Lite,
        Some(agentim_cluster::join_and_start),
    )
    .await
}
