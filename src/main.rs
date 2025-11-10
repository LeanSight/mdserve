mod app;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

use app::serve_markdown;

#[derive(Parser)]
#[command(name = "mdserve")]
#[command(about = "Fast markdown preview server with live reload and directory browsing")]
#[command(version)]
struct Args {
    /// Path to markdown file or directory to serve
    path: PathBuf,

    /// Hostname (domain or IP address) to listen on
    #[arg(short = 'H', long, default_value = "0.0.0.0")]
    hostname: String,

    /// Port to serve on
    #[arg(short, long, default_value = "3000")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Canonicalize the path once for consistent absolute path display
    let absolute_path = args.path.canonicalize().unwrap_or(args.path);

    serve_markdown(absolute_path, args.hostname, args.port).await?;

    Ok(())
}
