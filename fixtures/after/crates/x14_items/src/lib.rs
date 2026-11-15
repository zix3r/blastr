//! Every kind of item.

#[macro_export]
macro_rules! describe {
    ($value:expr) => {
        $value.to_string()
    };
}

/// A nested module.
pub mod nested {
    /// A nested helper.
    pub fn helper() -> u32 {
        1
    }
}

/// A pair of numbers.
pub struct Pair(pub u32, pub u32);

/// Number bits.
pub union Bits {
    /// The bits as an integer.
    pub whole: u32,
    /// The bits as a float.
    pub part: f32,
}

/// Work with values.
pub trait Work {
    /// A required method.
    fn run(&self) -> u32;

    /// A provided method.
    fn twice(&self) -> u32 {
        self.run() * 2
    }
}

/// A counter.
pub struct Counter {
    pub value: u32,
}

impl Counter {
    /// The starting value.
    pub const START: u32 = 0;

    /// Builds a counter without `self`.
    pub fn new() -> Counter {
        Counter {
            value: Counter::START,
        }
    }
}

/// A message.
pub enum Message {
    /// Stop.
    Quit,
    /// Move by a delta.
    Move { x: u32, y: u32 },
    /// Write text.
    Write(String),
}
