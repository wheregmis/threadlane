pub mod chat;
pub mod editor;
pub mod github;
pub mod right_panel;
pub mod settings;
pub mod sidebar;
pub mod terminal;
pub mod workspace;

pub(crate) async fn next_event_batch<T>(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
) -> Option<Vec<T>> {
    let mut events = vec![receiver.recv().await?];
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    Some(events)
}

#[cfg(test)]
mod tests {
    use super::next_event_batch;

    #[tokio::test]
    async fn event_batch_waits_for_one_event_then_drains_ready_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(1).unwrap();
        tx.send(2).unwrap();

        assert_eq!(next_event_batch(&mut rx).await, Some(vec![1, 2]));
    }
}
