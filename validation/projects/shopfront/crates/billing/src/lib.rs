//! Pricing policy for the shopfront: line totals, reductions, tax by region,
//! shipping quotes and the invoice renderer.
//!
//! Every number is in cents; nothing here rounds in floating point.

pub mod discount;
pub mod invoice;
pub mod pricing;
pub mod shipping;
pub mod tax;

pub use discount::{apply as apply_discount, describe as describe_discount, rebate};
pub use invoice::{generate_invoice, invoice_summary};
pub use pricing::{Totals, line_gross, line_total, order_subtotal, order_totals};
pub use shipping::shipping_cost;
pub use tax::{TAX_TABLE, TaxBracket, tax_for, tax_rate_bp};

/// Flat handling charge added to every quoted parcel.
pub const BASE_SHIPPING_CENTS: Cents = 499;

/// Charge per started kilogram of parcel weight.
pub const PER_KG_SHIPPING_CENTS: Cents = 120;

/// Added to parcels leaving the domestic zone.
pub const INTERNATIONAL_SURCHARGE_CENTS: Cents = 700;

/// Added once when a parcel carries hazardous goods.
pub const HAZARD_SURCHARGE_CENTS: Cents = 850;

/// Orders at or above this net amount ship for free.
pub const FREE_SHIPPING_THRESHOLD_CENTS: Cents = 7_500;

/// Most units of a single article a line may carry.
pub const MAX_LINE_QUANTITY: u32 = 99;

use domain::Cents;
