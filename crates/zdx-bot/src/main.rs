use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    zdx_bot::run().await
}
