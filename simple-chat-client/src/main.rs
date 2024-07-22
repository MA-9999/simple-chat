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

#[cfg(test)]
mod test {
    use simple_chat_shared::ClientServerMessages;

    use simple_chat_client::convert_string_to_command;

    #[tokio::test]
    async fn test_valid_string_to_command() {
        let username = "Test";
        let command_message = "send This is a test message";

        let command_result = convert_string_to_command(username, command_message).unwrap();

        assert_eq!(
            command_result,
            ClientServerMessages::Message(
                username.to_string(),
                "This is a test message".to_string()
            )
        )
    }

    #[tokio::test]
    async fn test_invalid_string_to_command() {
        let username = "Test";
        let command_message = "Test This is a test message";

        convert_string_to_command(username, command_message).unwrap_err();
    }
}
