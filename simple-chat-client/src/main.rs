use clap::Parser;
use log::info;
use simple_chat_client::*;
use std::process::exit;

#[tokio::main]
async fn main() {
    // Initialise logging
    env_logger::init();

    let args = Args::parse();

    info!(
        "Connecting to chat server on {}:{} with username {}",
        args.ip, args.port, &args.username
    );

    client_app(args).await;

    exit(0);
}
