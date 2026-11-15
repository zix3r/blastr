//! `blastr cache status` and `blastr cache clear`.

use blastr_core::Engine;

use crate::exit::Exit;
use crate::output;

/// Shows what the cache contains.
pub fn status() -> Result<Exit, blastr_core::Error> {
    let engine = Engine::open()?;
    let status = engine.cache_status()?;
    super::write_stdout(&output::render_cache_status(&status))?;
    Ok(Exit::Success)
}

/// Removes everything from the cache and reports what was removed.
pub fn clear() -> Result<Exit, blastr_core::Error> {
    let engine = Engine::open()?;
    let removed = engine.cache_clear()?;
    super::write_stdout(&output::render_cache_cleared(&removed))?;
    Ok(Exit::Success)
}
