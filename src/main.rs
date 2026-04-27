use anyhow::{Context, Result};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    carryover::cli::run().await.context("carryover failed")
}
