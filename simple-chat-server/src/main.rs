use clap::Parser;
use log::info;
use simple_chat_server::*;

#[tokio::main]
async fn main() {
    // Initialise logging
    env_logger::init();

    // Get Cli arguments
    let args = Args::parse();
    let mut ip_string = "127.0.0.1".to_string();
    let mut port_string = "10000".to_string();

    if let Some(ip) = args.ip {
        ip_string = ip.to_string();
    }

    if let Some(port) = args.port {
        port_string = format!("{port}");
    }

    let socket_address = format!("{ip_string}:{port_string}");

    info!("Starting chat server on address {}", &socket_address);

    // Run server
    run_server(socket_address).await;
}
