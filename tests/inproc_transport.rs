use std::num::NonZeroUsize;

use wprs::protocols::wprs::raw_buffer::RawBufferHeader;
use wprs::protocols::wprs::raw_buffer::RawBufferKind;
use wprs::protocols::wprs::raw_buffer::RawBufferPayload;
use wprs::protocols::wprs::serializer::RecvType;
use wprs::protocols::wprs::serializer::SendType;
use wprs::protocols::wprs::serializer::new_inproc_serializer_pair;
use wprs::protocols::wprs::wayland::WlSurfaceId;
use wprs::utils::arc_slice::ArcSlice;
use wprs::utils::sharding_compression::CompressedShards;
use wprs::utils::sharding_compression::ShardingCompressor;

#[test]
fn inproc_raw_buffer_fast_path_moves_uncompressed_bytes() {
    let (server, mut client) =
        new_inproc_serializer_pair::<wprs::protocols::wprs::types::Request, wprs::protocols::wprs::types::Event>()
            .unwrap();
    let surface = WlSurfaceId(42);
    let bytes = vec![1u8, 2, 3, 4, 5];
    let payload = RawBufferPayload {
        surface,
        kind: RawBufferKind::FilteredBgra,
        shards: CompressedShards::single_uncompressed(bytes.clone()),
    };

    server.writer().send(SendType::RawBuffer(payload));

    let reader = client.reader().unwrap();
    let msg = reader.recv().unwrap();
    match msg {
        RecvType::RawBuffer(msg) => {
            assert_eq!(
                msg.header,
                RawBufferHeader {
                    version: RawBufferHeader::V2,
                    kind: RawBufferKind::FilteredBgra,
                    surface: Some(surface),
                }
            );
            assert_eq!(msg.bytes, bytes);
        },
        other => panic!("unexpected recv type: {other:?}"),
    }
}

#[test]
fn inproc_raw_buffer_decompresses_compressed_shards() {
    let (server, mut client) =
        new_inproc_serializer_pair::<wprs::protocols::wprs::types::Request, wprs::protocols::wprs::types::Event>()
            .unwrap();
    let surface = WlSurfaceId(7);
    let bytes = vec![7u8; 8192];

    let mut compressor = ShardingCompressor::new(NonZeroUsize::new(1).unwrap(), 1).unwrap();
    let shards = compressor
        .compress(NonZeroUsize::new(1).unwrap(), ArcSlice::new(bytes.clone()));

    let payload = RawBufferPayload {
        surface,
        kind: RawBufferKind::FilteredBgra,
        shards,
    };

    server.writer().send(SendType::RawBuffer(payload));

    let reader = client.reader().unwrap();
    let msg = reader.recv().unwrap();
    match msg {
        RecvType::RawBuffer(msg) => {
            assert_eq!(msg.header.surface, Some(surface));
            assert_eq!(msg.header.kind, RawBufferKind::FilteredBgra);
            assert_eq!(msg.bytes, bytes);
        },
        other => panic!("unexpected recv type: {other:?}"),
    }
}
