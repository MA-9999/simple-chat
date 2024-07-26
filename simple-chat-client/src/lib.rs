use anyhow::Result;
use clap::Parser;
use futures_util::SinkExt;
use log::error;
use simple_chat_shared::ClientServerMessages;
use std::{net::IpAddr, string::String};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    spawn,
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::codec::{Framed, FramedRead, FramedWrite, LinesCodec, LinesCodecError};

pub const CHANNEL_SIZE: usize = 100;

#[derive(Parser)]
#[command(version, about, long_about)]
pub struct Args {
    /// Server ip address to connect to.
    #[arg(short, long)]
    pub ip: IpAddr,

    /// Server port to connect to.
    #[arg(short, long)]
    pub port: u16,

    /// Username to connect to the server with.
    #[arg(short, long)]
    pub username: String,
}

// This function handles messages being sent to and from the server.
pub async fn process_server_connection<A: ToSocketAddrs>(
    address: A,
    username: String,
    incoming_messages_tx: Sender<ClientServerMessages>,
    mut outgoing_messages_rx: Receiver<ClientServerMessages>,
) -> Result<()> {
    let stream = TcpStream::connect(address).await?;
    let mut user_message_stream = Framed::new(stream, LinesCodec::new());
    let mut run_loop = true;

    let join_command = serde_json::to_string(&ClientServerMessages::Join(username))?;

    // First send join message to the server.
    user_message_stream.send(join_command).await?;

    loop {
        tokio::select! {
            outgoing_message = outgoing_messages_rx.recv() => {
                match outgoing_message {
                    Some(message) => {
                        if let Ok(command_string) = serde_json::to_string(&message) {
                            user_message_stream.send(command_string).await?;
                        }
                    }
                    None => {
                        run_loop = false;
                    }
                }
            }

            incoming_message = user_message_stream.next() => {
                match incoming_message {
                    Some(json_message_result) => {
                        match json_message_result {
                            Ok(json_message) => {
                                if let Ok(message) = serde_json::from_str::<ClientServerMessages>(&json_message) {
                                    incoming_messages_tx.send(message).await?;
                                }
                                else {
                                    error!("Invalid message received from serer");
                                }
                            }
                            Err(error) => {
                                match error {
                                    LinesCodecError::MaxLineLengthExceeded => {

                                    }
                                    LinesCodecError::Io(_io_error) => {

                                    }
                                }
                            }
                        }
                    }
                    None => {
                        run_loop = false;
                    }
                }
            }
        }

        if !run_loop {
            break;
        }
    }

    Ok(())
}

// This function takes input lines from the terminal, and either returns a ClientServerMessage or an error.
pub fn convert_string_to_command(
    username: &str,
    command_string: &str,
) -> Result<ClientServerMessages> {
    let input_message_parts: Vec<&str> = command_string.splitn(2, ' ').collect();

    let command = input_message_parts[0];

    if !command.is_empty() {
        match command {
            "send" => {
                let message = input_message_parts[1];

                if !message.is_empty() {
                    Ok(ClientServerMessages::Message(
                        username.to_string(),
                        message.to_string(),
                    ))
                } else {
                    Err(anyhow::Error::msg("No message found with send command"))
                }
            }
            "leave" => Ok(ClientServerMessages::Leave(username.to_string())),
            _ => Err(anyhow::Error::msg(format!(
                "Invalid command {command} received from user"
            ))),
        }
    } else {
        Err(anyhow::Error::msg("Unable to split input message"))
    }
}

// This function handles the messages being from and written to the terminal and validates that the messages being sent by the user are valid.
pub async fn process_terminal(
    username: String,
    mut incoming_messages_rx: Receiver<ClientServerMessages>,
    outgoing_messages_tx: Sender<ClientServerMessages>,
) -> Result<()> {
    let input = tokio::io::stdin();
    let output = tokio::io::stdout();
    let mut input_stream = FramedRead::new(input, LinesCodec::new());
    let mut output_stream = FramedWrite::new(output, LinesCodec::new());
    let mut run_loop = true;

    loop {
        tokio::select! {
            input_message = input_stream.next() => {
                match input_message {
                    Some(input_message) => {
                        match input_message {
                            Ok(input_message) => {
                                match convert_string_to_command(&username, &input_message) {
                                    Ok(command) => {
                                        outgoing_messages_tx.send(command).await?
                                    }
                                    Err(error) => {
                                        error!("{}", error.to_string());
                                    }
                                }
                            }
                            Err(error) => {
                                match error {
                                    LinesCodecError::MaxLineLengthExceeded => {
                                        error!("{}", error.to_string());
                                    }
                                    LinesCodecError::Io(_io_error) => {
                                        run_loop = false;
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        run_loop = false;
                    }
                }
            },

            output_message = incoming_messages_rx.recv() => {
                match output_message {
                    Some(output_message) => {
                        match output_message {
                            ClientServerMessages::Join(username) => {
                                output_stream.send(format!("{username}: joined the server")).await?;
                            }
                            ClientServerMessages::Leave(username) => {
                                output_stream.send(format!("{username}: left the server")).await?
                            }
                            ClientServerMessages::Message(username, message) => {
                                output_stream.send(format!("{username}: {message}")).await?;
                            }
                            ClientServerMessages::Error(error_message) => {
                                output_stream.send(format!("Error: {error_message}")).await?;

                                run_loop = false;
                            }
                        }
                    }
                    None => {
                        run_loop = false;
                    }
                }
            },
        }

        if !run_loop {
            break;
        }
    }

    Ok(())
}

pub async fn client_app(args: Args) {
    let (incoming_messages_tx, incoming_messages_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);
    let (outgoing_messages_tx, outgoing_messages_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);

    let socket_address = format!("{}:{}", args.ip, args.port);
    let username_terminal = args.username.clone();
    let username_server = args.username.clone();

    // Task to handle messages to and from the server
    let process_server_handle = spawn(async move {
        let _ = process_server_connection(
            socket_address,
            username_server,
            incoming_messages_tx,
            outgoing_messages_rx,
        )
        .await;
    });

    // Task to handle terminal UI.
    let process_terminal = spawn(async move {
        let _ = process_terminal(
            username_terminal,
            incoming_messages_rx,
            outgoing_messages_tx,
        )
        .await;
    });

    let _ = process_server_handle.await;
    let _ = process_terminal.await;
}

#[cfg(test)]
mod test {
    use simple_chat_shared::ClientServerMessages;

    use crate::convert_string_to_command;

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
