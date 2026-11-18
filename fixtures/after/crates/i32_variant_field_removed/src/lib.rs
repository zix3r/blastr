//! An enum variant that loses one of its fields.

/// Who carries the parcel.
pub enum Carrier {
    /// Handed over at the counter.
    Pickup,
    /// Driven to the address.
    Courier {
        /// The courier company.
        name: String,
    },
}

/// The company that carries the parcel; names the field that stays.
pub fn tag(carrier: &Carrier) -> &str {
    match carrier {
        Carrier::Pickup => "pickup",
        Carrier::Courier { name, .. } => name.as_str(),
    }
}

/// Whether the parcel is promised for tomorrow, which nothing records now.
pub fn is_express(carrier: &Carrier) -> bool {
    let _ = carrier;
    false
}

/// Describes the speed; names no field of a courier.
pub fn speed(carrier: &Carrier) -> &'static str {
    match carrier {
        Carrier::Courier { .. } => "next day",
        Carrier::Pickup => "same day",
    }
}

/// Builds a next-day courier; spells the remaining field out.
pub fn express_courier(name: &str) -> Carrier {
    Carrier::Courier {
        name: name.to_string(),
    }
}
