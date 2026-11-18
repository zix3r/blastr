//! `macro_rules!` macros exported by `macrokit`.
//!
//! They are declared before every other module so the rest of the crate can use
//! them without `use` statements; `#[macro_export]` additionally makes them
//! available as `macrokit::ensure!` and friends.

/// Returns `Err($error)` from the enclosing function unless the condition holds.
///
/// The second arm is a `let ... else` guard:
/// `ensure!(let Some(value) = lookup(), otherwise ConfigError::..)`.
#[macro_export]
macro_rules! ensure {
    ($condition:expr, $error:expr $(,)?) => {
        if !$condition {
            return ::core::result::Result::Err($error);
        }
    };
    (let $pattern:pat = $expression:expr, otherwise $error:expr $(,)?) => {
        let $pattern = $expression else {
            return ::core::result::Result::Err($error);
        };
    };
}

/// Declares a set of `u32` newtype identifiers.
///
/// Each entry gets the standard derive set (plus `Display`, `From<u32>` and the
/// usual accessors) and a `label()` used when rendering.
#[macro_export]
macro_rules! newtype_ids {
    ($($(#[$meta:meta])* $name:ident => $label:literal),* $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
            pub struct $name(u32);

            impl $name {
                /// Placeholder used before a schema assigns real identifiers.
                pub const UNASSIGNED_LIMIT: u32 = u32::MAX;

                /// The unassigned placeholder identifier.
                pub const fn unassigned() -> Self {
                    Self(Self::UNASSIGNED_LIMIT)
                }

                /// Wraps a raw index.
                pub const fn new(index: u32) -> Self {
                    Self(index)
                }

                /// The raw index.
                pub const fn get(self) -> u32 {
                    self.0
                }

                /// Whether the schema has assigned this identifier yet.
                pub const fn is_unassigned(self) -> bool {
                    self.0 == Self::UNASSIGNED_LIMIT
                }

                /// Human readable prefix used by `Display`.
                pub const fn label() -> &'static str {
                    $label
                }
            }

            impl ::core::fmt::Display for $name {
                fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    write!(formatter, "{}{}", $label, self.0)
                }
            }

            impl ::core::convert::From<u32> for $name {
                fn from(index: u32) -> Self {
                    Self(index)
                }
            }
        )*
    };
}

/// Declares a static slice of [`CommandSpec`](crate::commands::CommandSpec)
/// values, usually built from the constants that `#[command]` generates.
#[macro_export]
macro_rules! command_table {
    ($(#[$meta:meta])* $visibility:vis $name:ident = [$($specification:path),* $(,)?] $(;)?) => {
        $(#[$meta])*
        $visibility static $name: &[$crate::commands::CommandSpec] = &[$($specification),*];
    };
}
