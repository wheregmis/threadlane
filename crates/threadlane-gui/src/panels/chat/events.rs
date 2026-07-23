//! Chat panel specific events and actions.

#[derive(Clone, Debug)]
pub enum ChatAction {
    SendMessage(String),
    ClearHistory,
    CancelGeneration,
}
