use simple_chat_shared::ClientServerMessages;
use tokio::sync::mpsc::channel;

const CHANNEL_SIZE: usize = 100;

#[tokio::test]
async fn test_two_clients() {
    let socket_address = "127.0.0.1:10000";
    let username_1 = "Test_Username_1";
    let username_2 = "Test_Username_2";

    let (incoming_messages_1_tx, mut incoming_messages_1_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);
    let (outgoing_messages_1_tx, outgoing_messages_1_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);

    let (incoming_messages_2_tx, mut incoming_messages_2_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);
    let (outgoing_messages_2_tx, outgoing_messages_2_rx) =
        channel::<simple_chat_shared::ClientServerMessages>(CHANNEL_SIZE);

    let _ = tokio::spawn(async move {
        simple_chat_server::run_server(socket_address.to_string()).await;
    });

    let _ = tokio::spawn(async move {
        simple_chat_client::process_server_connection(
            socket_address,
            username_1.to_string(),
            incoming_messages_1_tx,
            outgoing_messages_1_rx,
        )
        .await
        .unwrap();
    });

    // Give client 1 time to start before client 2, so that it always receives a join message from client 2, rather than the other way round.
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    let _ = tokio::spawn(async move {
        simple_chat_client::process_server_connection(
            socket_address,
            username_2.to_string(),
            incoming_messages_2_tx,
            outgoing_messages_2_rx,
        )
        .await
        .unwrap();
    });

    let join_message = incoming_messages_1_rx.recv().await.unwrap();

    // Check join message from client 2.
    assert_eq!(
        join_message,
        ClientServerMessages::Join(username_2.to_string())
    );

    let out_message_1 =
        ClientServerMessages::Message(username_1.to_string(), "Test message 1".to_string());

    outgoing_messages_1_tx
        .send(out_message_1.clone())
        .await
        .unwrap();
    let in_message_1 = incoming_messages_2_rx.recv().await.unwrap();

    // Check that the message received by client 2 is the same as the message sent by client 1.
    assert_eq!(out_message_1, in_message_1);

    let out_message_2 =
        ClientServerMessages::Message(username_2.to_string(), "Test message 2".to_string());

    outgoing_messages_2_tx
        .send(out_message_2.clone())
        .await
        .unwrap();
    let in_message_2 = incoming_messages_1_rx.recv().await.unwrap();

    // Check that the message received by client 1 is the same as the message sent by client 2.
    assert_eq!(out_message_2, in_message_2);

    let leave_message_1 = ClientServerMessages::Leave(username_1.to_string());
    outgoing_messages_1_tx.send(leave_message_1).await.unwrap();

    let leave_message_2 = ClientServerMessages::Leave(username_2.to_string());
    outgoing_messages_2_tx.send(leave_message_2).await.unwrap();
}
