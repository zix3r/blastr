//! The on-disk cache of revision graphs and finished analyses.
//!
//! Every entry is one file, `<root>/<kind>/<key>.bin`, whose name is the
//! hash of what it describes. A file is written beside its final name and
//! renamed into place, so a reader never sees half of one.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// What a cache entry holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The graph of one revision.
    Graph,
    /// The analysis of one comparison.
    Result,
}

impl Kind {
    /// The directory the entries of this kind live in.
    fn dir(self) -> &'static str {
        match self {
            Kind::Graph => "graphs",
            Kind::Result => "results",
        }
    }
}

/// File extension of a finished entry.
const EXTENSION: &str = "bin";

/// A cache rooted in one directory.
#[derive(Debug, Clone)]
pub(crate) struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache below `root`; nothing is created until the first write.
    pub(crate) fn new(root: &Path) -> Cache {
        Cache {
            root: root.to_path_buf(),
        }
    }

    /// The file of one entry.
    fn file(&self, kind: Kind, key: &str) -> PathBuf {
        self.root
            .join(kind.dir())
            .join(format!("{key}.{EXTENSION}"))
    }

    /// The bytes stored under `key`, or `None` when there are none.
    pub(crate) fn get(&self, kind: Kind, key: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.file(kind, key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether anything is stored under `key`.
    pub(crate) fn contains(&self, kind: Kind, key: &str) -> bool {
        self.file(kind, key).is_file()
    }

    /// Stores `bytes` under `key`, replacing what was there.
    pub(crate) fn put(&self, kind: Kind, key: &str, bytes: &[u8]) -> io::Result<()> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let target = self.file(kind, key);
        let dir = self.root.join(kind.dir());
        std::fs::create_dir_all(&dir)?;
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = dir.join(format!("{key}.{}-{unique}.tmp", std::process::id()));
        std::fs::write(&temporary, bytes)?;
        match std::fs::rename(&temporary, &target) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                // Another writer got there first with the same content; a
                // reader holding the file open can stop the replacement on
                // Windows.
                if target.is_file() { Ok(()) } else { Err(error) }
            }
        }
    }

    /// The number of entries of `kind` and their total size in bytes.
    pub(crate) fn stats(&self, kind: Kind) -> io::Result<(u64, u64)> {
        let entries = match std::fs::read_dir(self.root.join(kind.dir())) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((0, 0)),
            Err(error) => return Err(error),
        };
        let (mut count, mut bytes) = (0, 0);
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == EXTENSION)
            {
                count += 1;
                bytes += entry.metadata()?.len();
            }
        }
        Ok((count, bytes))
    }

    /// Removes every entry.
    pub(crate) fn clear(&self) -> io::Result<()> {
        for kind in [Kind::Graph, Kind::Result] {
            match std::fs::remove_dir_all(self.root.join(kind.dir())) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_are_counted_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path());
        assert_eq!(cache.get(Kind::Graph, "a").unwrap(), None);
        assert!(!cache.contains(Kind::Graph, "a"));

        cache.put(Kind::Graph, "a", b"graph").unwrap();
        cache.put(Kind::Graph, "a", b"graph2").unwrap();
        cache.put(Kind::Result, "b", b"result").unwrap();
        assert_eq!(
            cache.get(Kind::Graph, "a").unwrap().as_deref(),
            Some(&b"graph2"[..])
        );
        assert!(cache.contains(Kind::Result, "b"));
        assert!(!cache.contains(Kind::Graph, "b"));
        assert_eq!(cache.stats(Kind::Graph).unwrap(), (1, 6));
        assert_eq!(cache.stats(Kind::Result).unwrap(), (1, 6));

        cache.clear().unwrap();
        assert_eq!(cache.stats(Kind::Graph).unwrap(), (0, 0));
        assert_eq!(cache.get(Kind::Result, "b").unwrap(), None);
    }
}
