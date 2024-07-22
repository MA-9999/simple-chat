use anyhow::Result;
use clap::Parser;
use futures_util::SinkExt;
use log::{debug, error};
use simple_chat_shared::ClientServerMessages;
use std::{collections::HashMap, fmt::Debug, net::IpAddr, string::String, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        mpsc::{channel, Receiver, Sender},
        RwLock,
    },
};
use tokio_stream::{wrappers::TcpListenerStream, StreamExt};
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

const CHANNEL_SIZE: usize = 100;

#[derive(Parser)]
#[command(version, about, long_about)]
pub struct Args {
    /// Set the server ip to listen on.
    #[arg(short)]
    pub ip: Option<IpAddr>,

    /// Set the server port to listen on.
    #[arg(short)]
    pub port: Option<u16>,
}

pub struct User<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> {
    // Tx half of mpsc channel.
    username: String,
    user_stream: Framed<T, LinesCodec>,
    user_messages_rx: Receiver<ClientServerMessages>,
    user_map: Arc<UserMap>,
    user_map_updates_tx: Sender<(ClientServerMessages, Option<Framed<T, LinesCodec>>)>,
}

impl<T> User<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(
        username: String,
        user_stream: Framed<T, LinesCodec>,
        user_messages_rx: Receiver<ClientServerMessages>,
        user_map: Arc<UserMap>,
        user_map_updates_tx: Sender<(ClientServerMessages, Option<Framed<T, LinesCodec>>)>,
    ) -> Self {
        Self {
            username,
            user_stream,
            user_messages_rx,
            user_map,
            user_map_updates_tx,
        }
    }
}

// This function pushes messages from one user to all other users on the server.
pub async fn push_message(
    user_map: Arc<UserMap>,
    sending_username: &str,
    message: &ClientServerMessages,
) -> Result<()> {
    let users = user_map.users.read().await;

    for (username, sender) in users.iter() {
        if *username != sending_username {
            if let Err(error) = sender.send(message.clone()).await {
                return Err(anyhow::Error::msg(format!(
                    "Error {error} sending message from user {} to user {username}",
                    sending_username
                )));
            }
        }
    }

    Ok(())
}

// This function runs as a task for each user - handling messages to and from the client.
pub async fn process_user_updates<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut user: User<T>,
) {
    let mut run_loop = true;

    // Push message to let other users know that the user has joined the server.
    if let Err(error) = push_message(
        user.user_map.clone(),
        &user.username,
        &ClientServerMessages::Join(user.username.clone()),
    )
    .await
    {
        error!("{}", error.to_string());
    }

    // Run tokio select! over both receiving messages from TcpStream to process + messages from other users to send to the client
    loop {
        tokio::select! (
            // Handle incoming messages from the client
            incoming_message = user.user_stream.next() => {
                match incoming_message {
                    Some(Ok(json_message)) => {
                        let message_result = serde_json::from_str::<ClientServerMessages>(&json_message);

                        if let Ok(message) = message_result {
                            debug!("Message received from user {}: {:?}", user.username.clone(), message.clone());

                            match message.clone() {
                                ClientServerMessages::Message(..) => {
                                    if let Err(error) = push_message(user.user_map.clone(), &user.username, &message).await {
                                        error!("{}", error.to_string());
                                    }
                                }
                                ClientServerMessages::Leave(username) => {
                                    if let Err(error) = push_message(user.user_map.clone(), &user.username, &message).await {
                                        error!("{}", error.to_string());
                                    }

                                    if let Err(error) = user.user_map_updates_tx.send((message.clone(), None)).await {
                                        // TODO: Log error
                                        error!("Error {error} when passing leave message for user {username} to user map task");
                                    }

                                    run_loop = false;
                                }

                                // We should only be getting Leave or Message - anything else should be logged as an error.
                                _ => {
                                    error!("Message {message:?} received from user {} when we should only receive Leave or Message messages", user.username);
                                }
                            }
                        } else {
                            // If we are unable to deserialise the string to a ClientServerMessage then log it as an error.
                            error!("Invalid message {json_message:?} sent from user {}", user.username);
                        }
                    }

                    Some(Err(error)) => {
                        match error {
                            LinesCodecError::MaxLineLengthExceeded => {
                                error!("Error {error} when trying to receive message from tcp stream");
                            }
                            LinesCodecError::Io(error) => {
                                debug!("IO Error {error} when trying to receive message from tcp stream");

                                if let Err(error) = deregister_user(user.user_map.clone(), user.username.clone()).await {
                                    error!("{}", error);
                                }

                                run_loop = false;
                            }
                        }
                    }

                    None => {
                        debug!("Tcp stream has finished for user {}", user.username.clone());

                        if let Err(error) = deregister_user(user.user_map.clone(), user.username.clone()).await {
                            error!("{}", error);
                        }

                        run_loop = false;
                    }
                }

            },

            // Handle messages being sent to the client.
            outgoing_message = user.user_messages_rx.recv() => {
                if let Some(message) = outgoing_message {
                    match serde_json::to_string(&message) {
                        Ok(message_string) => {
                            if let Err(error) = user.user_stream.send(message_string).await {
                                error!("Error {error} sending message {:?}", message);
                            }
                        }
                        Err(error) => {
                            error!("Error {error} converting message {message:?} to json");
                        }
                    }
                }
            },
        );

        if !run_loop {
            break;
        }
    }
}

pub struct UserMap {
    // Rwlock of hashmap of users, using usernames as a key
    users: RwLock<HashMap<String, Sender<ClientServerMessages>>>,
}

impl UserMap {
    pub fn new() -> Self {
        Self {
            users: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for UserMap {
    fn default() -> Self {
        Self::new()
    }
}

// This function adds a new user to the user map if there isn't already a user with the same username, and then spawns a new task to handle messages to and from the user.
pub async fn register_new_user<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    user_map_arc: Arc<UserMap>,
    username: String,
    mut user_stream: Framed<T, LinesCodec>,
    user_map_updates_tx: Sender<(ClientServerMessages, Option<Framed<T, LinesCodec>>)>,
) -> Result<()> {
    let (user_messages_tx, user_messages_rx) = channel::<ClientServerMessages>(CHANNEL_SIZE);

    let user_map = user_map_arc.clone();
    let mut user_map = user_map.users.write().await;

    // If the username is already in use, send a message to the client, telling them to use another username.
    if user_map.get(&username).is_some() {
        let error_string =
            format!("Username {username} already in use. Please use another username");

        if let Ok(username_error_message) = serde_json::to_string::<ClientServerMessages>(
            &ClientServerMessages::Error(error_string.clone()),
        ) {
            if let Err(error) = user_stream.send(username_error_message).await {
                return Err(anyhow::Error::msg(format!(
                    "Error {error} sending username error message to user {username}"
                )));
            }
        }

        return Err(anyhow::Error::msg(error_string));
    } else {
        match user_map.insert(username.clone(), user_messages_tx) {
            Some(..) => {
                // This shouldn't happen - log as error
                return Err(anyhow::Error::msg(format!("Existing value found for user {username} when inserting new value after get call found no existing value")));
            }

            None => {
                let user = User::new(
                    username,
                    user_stream,
                    user_messages_rx,
                    user_map_arc.clone(),
                    user_map_updates_tx,
                );

                // Spawn task to handle messages for this user.
                tokio::spawn(async move { process_user_updates(user).await });
            }
        }
    }

    Ok(())
}

// This function removes an existing user from the user map.
pub async fn deregister_user(user_map: Arc<UserMap>, username: String) -> Result<()> {
    let mut user_map = user_map.users.write().await;

    if user_map.get(&username).is_some() {
        user_map.remove(&username);
    } else {
        // TODO: If we can't deregister the user, then we should log the error.
        return Err(anyhow::Error::msg(format!(
            "Unable to find user {username} in the user map"
        )));
    }

    Ok(())
}

// This function runs as a task handling messages to register and deregister users.
// New users can be added with a join message from a client, and should fail if the username in question is already in use, with a message sent back to the client.
// Existing users can be deregistered either by the client sending a leave message, or by disconnecting the TcpStream.
pub async fn process_user_map_updates<
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static + Debug,
>(
    user_map: Arc<UserMap>,
    user_map_updates_tx: Sender<(ClientServerMessages, Option<Framed<T, LinesCodec>>)>,
    mut user_map_updates_rx: Receiver<(ClientServerMessages, Option<Framed<T, LinesCodec>>)>,
) -> Result<()> {
    loop {
        let update_message = match user_map_updates_rx.recv().await {
            Some(update_message) => update_message,
            None => {
                break;
            }
        };

        match update_message {
            (ClientServerMessages::Join(username), Some(user_stream)) => {
                if let Err(error) = register_new_user(
                    user_map.clone(),
                    username,
                    user_stream,
                    user_map_updates_tx.clone(),
                )
                .await
                {
                    error!("{}", error);
                }
            }
            (ClientServerMessages::Leave(username), None) => {
                if let Err(error) = deregister_user(user_map.clone(), username).await {
                    error!("{}", error);
                }
            }

            // Any other type of message sent to this task is an error.
            _ => {
                // TODO: Log error
                error!("Invalid ClientServerMessage {update_message:?} received by process_user_map_updates()");
            }
        }
    }

    Ok(())
}

// This function accepts new connections
pub async fn accept_new_connections<A: ToSocketAddrs>(
    socket_address: A,
    user_map_updates_tx: Sender<(ClientServerMessages, Option<Framed<TcpStream, LinesCodec>>)>,
) -> Result<()> {
    let server_listener = TcpListener::bind(socket_address).await?;
    let mut listener_stream = TcpListenerStream::new(server_listener);

    // Wait for new connections from clients.
    while let Some(connection) = listener_stream.next().await {
        if let Ok(connection) = connection {
            let mut user_message_stream = Framed::new(connection, LinesCodec::new());

            match user_message_stream.next().await {
                Some(Ok(message)) => {
                    let message_result = serde_json::from_str::<ClientServerMessages>(&message);

                    if let Ok(message) = message_result {
                        match message.clone() {
                            ClientServerMessages::Join(username) => {
                                if let Err(error) = user_map_updates_tx
                                    .send((message.clone(), Some(user_message_stream)))
                                    .await
                                {
                                    // TODO: Log error
                                    error!("Error {error} when passing join message for user {username} to user map task");
                                }
                            }

                            // Only join messages should be received at this point - anything else should be logged as an error.
                            _ => {
                                // TODO: Log error
                                error!("ClientServerMessage {message:?} received before user has been registered");
                            }
                        }
                    }
                }
                Some(Err(error)) => match error {
                    LinesCodecError::MaxLineLengthExceeded => {
                        error!("{}", error.to_string());
                    }
                    LinesCodecError::Io(error) => {
                        debug!("IO Error {error} when trying to receive message from tcp stream");
                    }
                },
                None => {
                    debug!("Tcp stream has finished");
                }
            }
        } else {
            // TODO: Log
        }
    }

    Ok(())
}

pub async fn run_server(socket_address: String) {
    let user_map: Arc<UserMap> = Arc::new(UserMap::new());

    let (user_map_updates_tx, user_map_updates_rx) =
        channel::<(ClientServerMessages, Option<Framed<TcpStream, LinesCodec>>)>(CHANNEL_SIZE);
    let user_map_listener = user_map_updates_tx.clone();

    // Spawn task to listen for new incoming new connections.
    let listener_task =
        tokio::spawn(
            async move { accept_new_connections(socket_address, user_map_listener).await },
        );

    // Spawn task to update the user map.
    let user_map_update_task = tokio::spawn(async move {
        process_user_map_updates(user_map.clone(), user_map_updates_tx, user_map_updates_rx).await
    });

    let _ = listener_task.await;
    let _ = user_map_update_task.await;
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use crate::{deregister_user, push_message, register_new_user, UserMap, CHANNEL_SIZE};
    use simple_chat_shared::ClientServerMessages;
    use tokio::{
        io::{duplex, DuplexStream},
        sync::mpsc::channel,
    };
    use tokio_util::codec::{Framed, LinesCodec};

    #[tokio::test]
    async fn register_and_deregister() {
        //let (stream_tx, stream_rx) =

        let (_client, server) = duplex(1024);

        let user_stream = Framed::new(server, LinesCodec::new());
        let (user_map_updates_tx, _user_map_updates_rx) = tokio::sync::mpsc::channel::<(
            ClientServerMessages,
            Option<Framed<DuplexStream, LinesCodec>>,
        )>(100);

        let user_map = Arc::new(UserMap::new());
        let username = "Test".to_string();

        register_new_user(
            user_map.clone(),
            username.clone(),
            user_stream,
            user_map_updates_tx,
        )
        .await
        .unwrap();
        deregister_user(user_map.clone(), username.clone())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn register_twice() {
        let (_client, server) = duplex(1024);
        let (_client_2, server_2) = duplex(1024);

        let user_stream = Framed::new(server, LinesCodec::new());
        let user_stream_2 = Framed::new(server_2, LinesCodec::new());
        let (user_map_updates_tx, _user_map_updates_rx) = tokio::sync::mpsc::channel::<(
            ClientServerMessages,
            Option<Framed<DuplexStream, LinesCodec>>,
        )>(100);

        let user_map = Arc::new(UserMap::new());
        let username = "Test".to_string();

        register_new_user(
            user_map.clone(),
            username.clone(),
            user_stream,
            user_map_updates_tx.clone(),
        )
        .await
        .unwrap();
        register_new_user(user_map, username, user_stream_2, user_map_updates_tx)
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn deregister_without_registering() {
        let user_map = Arc::new(UserMap::new());
        let username = "Test".to_string();

        deregister_user(user_map.clone(), username.clone())
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn register_multiple_users() {
        let (_client, server) = duplex(1024);
        let (_client_2, server_2) = duplex(1024);

        let user_stream = Framed::new(server, LinesCodec::new());
        let user_stream_2 = Framed::new(server_2, LinesCodec::new());
        let (user_map_updates_tx, _user_map_updates_rx) = tokio::sync::mpsc::channel::<(
            ClientServerMessages,
            Option<Framed<DuplexStream, LinesCodec>>,
        )>(CHANNEL_SIZE);

        let user_map = Arc::new(UserMap::new());
        let username = "Test".to_string();
        let username_2 = "Test_2".to_string();

        register_new_user(
            user_map.clone(),
            username,
            user_stream,
            user_map_updates_tx.clone(),
        )
        .await
        .unwrap();
        register_new_user(
            user_map.clone(),
            username_2,
            user_stream_2,
            user_map_updates_tx,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_push_message() {
        let user_map = Arc::new(UserMap::new());

        let username_1 = "Test_User_1";
        let username_2 = "Test_User_2";
        let (user_messages_tx, mut user_messages_rx) =
            channel::<ClientServerMessages>(CHANNEL_SIZE);

        {
            let user_map_handle = user_map.clone();
            let mut user_map_handle = user_map_handle.users.write().await;

            user_map_handle.insert(username_1.to_string(), user_messages_tx);
        }

        let message = ClientServerMessages::Message(
            username_2.to_string(),
            "This is a test message".to_string(),
        );

        push_message(user_map, username_2, &message).await.unwrap();

        let received_message = user_messages_rx.recv().await.unwrap();

        assert_eq!(message, received_message);
    }
}
