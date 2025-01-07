use iceoryx2::port::event_id::EventId;
use num_enum::TryFromPrimitive;

#[derive(Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(usize)]
pub enum IpcEvent {
    ServerConnected = 0,
    ServerDisconnected,
    ClientConnected,
    ClientDisconnected,

    RequestSent,
    RequestReceived,
    ResponseSent,
    ResponseReceived,

    ServerReady,
    ProcessDied,

    Unknown = u32::MAX as usize,
}

impl From<IpcEvent> for EventId {
    fn from(value: IpcEvent) -> Self {
        EventId::new(value as usize)
    }
}

impl From<EventId> for IpcEvent {
    fn from(value: EventId) -> Self {
        IpcEvent::try_from(value.as_value()).unwrap_or(IpcEvent::Unknown)
    }
}
