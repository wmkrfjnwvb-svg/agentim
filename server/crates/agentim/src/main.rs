//! AgentIM server: one program from a single node to a cluster.

#[tokio::main]
async fn main() {
    agentim_core::cli::run_with(Some(agentim_cluster::SUPPORT)).await
}
