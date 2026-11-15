//! An enum that gains a variant.

/// The kinds of event.
pub enum Event {
    /// Something started.
    Start,
    /// Something stopped.
    Stop,
    /// Something paused.
    Pause,
}

/// Names the event.
pub fn name(event: &Event) -> &'static str {
    match event {
        Event::Start => "start",
        Event::Stop => "stop",
        Event::Pause => "pause",
    }
}

/// Reports whether anything happened.
pub fn happened(event: &Event) -> bool {
    match event {
        Event::Start => true,
        _ => false,
    }
}

/// Logs the event.
pub fn log(event: &Event) -> String {
    format!("event: {}", happened(event))
}

/// Builds a stop event.
pub fn stop() -> Event {
    Event::Stop
}
