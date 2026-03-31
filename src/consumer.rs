//! PipeWire consumer presence events.

/// Notification of PipeWire consumer count change.
#[derive(Debug, Clone, Copy)]
pub enum ConsumerEvent {
    /// Consumer count changed. Payload is the new total.
    Changed(u32),
}
