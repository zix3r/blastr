//! Dispatch through a trait object and a generic bound.

/// A sound with a volume and a name.
pub trait Sound {
    /// The volume in units.
    fn volume(&self) -> u32;

    /// The name of the sound.
    fn label(&self) -> &'static str;
}

/// A drum.
pub struct Drum;

impl Sound for Drum {
    fn volume(&self) -> u32 {
        11
    }

    fn label(&self) -> &'static str {
        "drum"
    }
}

/// A bell.
pub struct Bell;

impl Sound for Bell {
    fn volume(&self) -> u32 {
        22
    }

    fn label(&self) -> &'static str {
        "bell"
    }
}

/// Reads the volume through a trait object.
pub fn through_object(sound: &dyn Sound) -> u32 {
    sound.volume()
}

/// Reads the volume through a generic bound.
pub fn through_generic<S: Sound>(sound: &S) -> u32 {
    sound.volume()
}

/// Reads a different trait method.
pub fn label_of(sound: &dyn Sound) -> &'static str {
    sound.label()
}
