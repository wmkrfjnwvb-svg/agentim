//! AgentIM: AgentIM Lite plus the platform modules (docs/01-product.md §6).

#[tokio::main]
async fn main() {
    agentim_core::cli::run_with(
        agentim_core::edition::Edition::Full,
        Some(agentim_cluster::join_and_start),
    )
    .await
}
