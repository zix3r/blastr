//! A trait that gains a required method.

/// Something that can be summed up.
pub trait Recap {
    /// The title of the summary.
    fn title(&self) -> &'static str;
}

/// Something that carries a tag.
pub trait Tagged {
    /// The tag number.
    fn tag(&self) -> u8;
}

/// A report.
pub struct Report;

impl Recap for Report {
    fn title(&self) -> &'static str {
        "report"
    }
}

/// A summary.
pub struct Summary;

impl Recap for Summary {
    fn title(&self) -> &'static str {
        "summary"
    }
}

impl Tagged for Report {
    fn tag(&self) -> u8 {
        7
    }
}
