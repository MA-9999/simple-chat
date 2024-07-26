use serde::{Deserialize, Serialize};
use std::string::String;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ClientServerMessages {
    Join(String),
    Leave(String),
    Message(String, String),
    Error(String),
}
