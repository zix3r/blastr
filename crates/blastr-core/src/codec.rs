//! Encoding of cache entries: revision graphs and finished analyses.
//!
//! A value is serialised with `postcard` and then compressed with `zstd`.
//! Every failure is reported as [`Error::Codec`], because a value that cannot
//! be encoded or decoded means the cache cannot be trusted, not that the
//! project under analysis is wrong.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

/// Compression level used for cache entries.
///
/// Entries compress very well and are read far more often than they are
/// written, so a middle level keeps both costs small.
const COMPRESSION_LEVEL: i32 = 3;

/// Encodes a value as postcard bytes compressed with zstd.
pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let raw = postcard::to_allocvec(value)
        .map_err(|error| Error::Codec(format!("could not serialise a cache entry: {error}")))?;
    zstd::encode_all(raw.as_slice(), COMPRESSION_LEVEL)
        .map_err(|error| Error::Codec(format!("could not compress a cache entry: {error}")))
}

/// Decodes bytes produced by [`encode`].
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let raw = zstd::decode_all(bytes)
        .map_err(|error| Error::Codec(format!("could not decompress a cache entry: {error}")))?;
    postcard::from_bytes(&raw)
        .map_err(|error| Error::Codec(format!("could not deserialise a cache entry: {error}")))
}

#[cfg(test)]
mod tests {
    use blastr_model::{
        Certainty, CrateId, CrateInfo, Edge, EdgeDetail, EdgeKind, Location, RelPath, Span, Symbol,
        SymbolFlags, SymbolId, SymbolKind, SymbolPath, TargetKind, UnresolvedRef, Visibility,
    };

    use super::{encode as encode_graph, *};
    use blastr_model::RevisionGraph;

    fn decode_graph(bytes: &[u8]) -> Result<RevisionGraph> {
        decode(bytes)
    }

    fn loc(file: &str, start: u32, end: u32) -> Location {
        Location {
            file: RelPath::new(file),
            span: Span {
                start,
                end,
                start_line: 1,
                end_line: 1,
            },
        }
    }

    fn symbol(path: &str, kind: SymbolKind, id: u32, parent: Option<SymbolId>) -> Symbol {
        Symbol {
            path: SymbolPath {
                path: path.to_owned(),
                kind,
                disambiguator: 0,
            },
            krate: CrateId(0),
            location: loc("src/lib.rs", id * 10, id * 10 + 5),
            visibility: Visibility::Public,
            signature_hash: 7,
            body_hash: 11,
            attr_hash: 0,
            body_sketch: vec![1, 2, 3],
            parent,
            flags: SymbolFlags {
                has_body: true,
                ..SymbolFlags::default()
            },
            impl_info: None,
        }
    }

    fn sample_graph() -> RevisionGraph {
        RevisionGraph {
            crates: vec![CrateInfo {
                name: "sample".to_owned(),
                package: "sample".to_owned(),
                target_kind: TargetKind::Lib,
                target_name: "sample".to_owned(),
                root_file: RelPath::new("src/lib.rs"),
            }],
            symbols: vec![
                symbol("sample", SymbolKind::Module, 0, None),
                symbol("sample::caller", SymbolKind::Function, 1, Some(SymbolId(0))),
                symbol("sample::callee", SymbolKind::Function, 2, Some(SymbolId(0))),
            ],
            edges: vec![Edge {
                from: SymbolId(1),
                to: SymbolId(2),
                kind: EdgeKind::Calls,
                certainty: Certainty::Resolved,
                site: loc("src/lib.rs", 30, 36),
                detail: EdgeDetail::None,
            }],
            unresolved: vec![UnresolvedRef {
                location: loc("src/lib.rs", 40, 44),
                text: "missing".to_owned(),
            }],
            inactive: vec![loc("src/lib.rs", 50, 60)],
            restricted: true,
        }
    }

    #[test]
    fn graph_round_trips() {
        let graph = sample_graph();
        let bytes = encode_graph(&graph).unwrap();
        assert!(!bytes.is_empty());
        let decoded = decode_graph(&bytes).unwrap();
        assert_eq!(decoded, graph);
    }

    #[test]
    fn empty_graph_round_trips() {
        let graph = RevisionGraph::default();
        let decoded = decode_graph(&encode_graph(&graph).unwrap()).unwrap();
        assert_eq!(decoded, graph);
    }

    #[test]
    fn garbage_is_a_codec_error() {
        let error = decode_graph(b"not a zstd frame at all").unwrap_err();
        assert!(matches!(error, Error::Codec(_)));
    }

    #[test]
    fn truncated_frame_is_a_codec_error() {
        let bytes = encode_graph(&sample_graph()).unwrap();
        let error = decode_graph(&bytes[..bytes.len() / 2]).unwrap_err();
        assert!(matches!(error, Error::Codec(_)));
    }

    #[test]
    fn valid_frame_with_bad_payload_is_a_codec_error() {
        let frame = zstd::encode_all(&b""[..], 3).unwrap();
        let error = decode_graph(&frame).unwrap_err();
        assert!(matches!(error, Error::Codec(_)));
    }
}
