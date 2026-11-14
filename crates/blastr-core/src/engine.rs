//! The entry point of the analysis pipeline.

use crate::Result;

/// Entry point of the analysis pipeline. Owns the cache.
#[derive(Debug)]
pub struct Engine {
    store: blastr_store::Store,
}

impl Engine {
    /// Opens the engine with the per-user cache database
    /// (`blastr_store::paths()?.database()`).
    pub fn open() -> Result<Engine> {
        let paths = blastr_store::paths()?;
        let store = blastr_store::Store::open(&paths.database())?;
        Ok(Engine::with_store(store))
    }

    /// Creates an engine on an already opened store (used by tests).
    pub fn with_store(store: blastr_store::Store) -> Engine {
        Engine { store }
    }

    /// The cache this engine uses.
    pub fn store(&self) -> &blastr_store::Store {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_uses_the_given_store() {
        let store = blastr_store::Store::open_in_memory().unwrap();
        let engine = Engine::with_store(store);
        let _ = engine.store();
    }
}
