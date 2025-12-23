// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt;
use std::fmt::Debug;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::net::Shutdown;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::num::NonZeroUsize;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use calloop::channel;
use calloop::channel::Channel;
use crossbeam_channel::Receiver;
use crossbeam_channel::RecvTimeoutError;
use crossbeam_channel::Sender;
#[cfg(unix)]
use nix::sys::socket;
#[cfg(unix)]
use nix::sys::socket::sockopt::RcvBuf;
#[cfg(unix)]
use nix::sys::socket::sockopt::SndBuf;
use num_enum::IntoPrimitive;
use num_enum::TryFromPrimitive;
use rkyv::Archive;
use rkyv::Deserialize;
use rkyv::Serialize;
use rkyv::api::high::HighDeserializer;
use rkyv::api::high::HighSerializer;
use rkyv::api::high::HighValidator;
use rkyv::bytecheck;
use rkyv::rancor::Error as RancorError;
use rkyv::ser::allocator::ArenaHandle;
use rkyv::util::AlignedVec;
#[cfg(unix)]
use sysctl::Ctl;
#[cfg(unix)]
use sysctl::Sysctl;

use crate::prelude::*;
use crate::utils;
use crate::utils::arc_slice::ArcSlice;
use crate::utils::channel::DiscardingSender;
use crate::utils::channel::InfallibleSender;
use crate::utils::sharding_compression::CompressedShards;
use crate::utils::sharding_compression::ShardingCompressor;
use crate::utils::sharding_compression::ShardingDecompressor;

use super::endpoint::Endpoint;
use super::endpoint::TransportGuard;
use super::endpoint::setup_client_transport;
use super::framing::Framed;
use super::raw_buffer::RawBufferHeader;
use super::raw_buffer::RawBufferMessage;
use super::raw_buffer::RawBufferPayload;
use super::raw_buffer::decompress_shards_to_owned;
use super::raw_buffer::extract_single_uncompressed_shard;

const CHANNEL_SIZE: usize = 1024;

pub trait Serializable:
    Debug
    + Send
    + Archive
    + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
    + 'static
{
}

impl<T> Serializable for T where
    T: Debug
        + Send
        + Archive
        + for<'a> Serialize<HighSerializer<AlignedVec, ArenaHandle<'a>, RancorError>>
        + 'static
{
}

const DEFAULT_SOCKET_BUFFER: usize = 4 * 1024 * 1024;

#[cfg(unix)]
fn socket_buffer_limits() -> Result<(usize, usize)> {
    let rmem_max = match Ctl::new("net.core.rmem_max").and_then(|c| c.value_string()) {
        Ok(v) => v,
        Err(_) => {
            debug!("sysctl net.core.rmem_max not available; using default");
            return Ok((DEFAULT_SOCKET_BUFFER, DEFAULT_SOCKET_BUFFER));
        },
    };
    let wmem_max = match Ctl::new("net.core.wmem_max").and_then(|c| c.value_string()) {
        Ok(v) => v,
        Err(_) => {
            debug!("sysctl net.core.wmem_max not available; using default");
            return Ok((DEFAULT_SOCKET_BUFFER, DEFAULT_SOCKET_BUFFER));
        },
    };

    let rmem_max: usize = rmem_max.parse().unwrap_or(DEFAULT_SOCKET_BUFFER);
    let wmem_max: usize = wmem_max.parse().unwrap_or(DEFAULT_SOCKET_BUFFER);
    Ok((rmem_max, wmem_max))
}

#[cfg(not(unix))]
fn socket_buffer_limits() -> Result<(usize, usize)> {
    Ok((DEFAULT_SOCKET_BUFFER, DEFAULT_SOCKET_BUFFER))
}

#[cfg(unix)]
fn enlarge_socket_buffer<F: AsFd>(fd: &F) {
    let (rmem_max, wmem_max) = warn_and_return!(socket_buffer_limits());

    socket::setsockopt(fd, RcvBuf, &rmem_max).warn_and_ignore(loc!());
    socket::setsockopt(fd, SndBuf, &wmem_max).warn_and_ignore(loc!());
}

#[cfg(not(unix))]
fn enlarge_socket_buffer<T>(_fd: &T) {}

trait CloneableStream: Read + Write + Send + 'static {
    fn clone_stream(&self) -> std::io::Result<Self>
    where
        Self: Sized;

    fn shutdown_both(&self) -> std::io::Result<()>;
}

#[cfg(unix)]
impl CloneableStream for UnixStream {
    fn clone_stream(&self) -> std::io::Result<Self> {
        UnixStream::try_clone(self)
    }

    fn shutdown_both(&self) -> std::io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

impl CloneableStream for TcpStream {
    fn clone_stream(&self) -> std::io::Result<Self> {
        TcpStream::try_clone(self)
    }

    fn shutdown_both(&self) -> std::io::Result<()> {
        self.shutdown(Shutdown::Both)
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct Version(String);

impl Version {
    fn new() -> Self {
        Self(env!("SERIALIZATION_TREE_HASH").to_string())
    }

    fn compare_and_warn(&self, other: &Self) {
        if self != other {
            warn!(
                "Self version is {:?}, while other version is {:?}. These versions may be incompatible; if you experience bugs (especially hanging or crashes), restart the server.",
                self, other
            );
        }
    }
}

impl Framed for Version {
    fn framed_write<W: Write>(&self, stream: &mut W) -> Result<()> {
        self.0.framed_write(stream)
    }

    fn framed_read<R: Read>(stream: &mut R) -> Result<Self> {
        Ok(Self(String::framed_read(stream).location(loc!())?))
    }
}

// TODO: figure out how to shorten the T::Archived bound. This may require
// https://github.com/rust-lang/rust/issues/52662.

pub enum SendType<ST>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    Object(ST),
    RawBuffer(RawBufferPayload),
}

impl<ST> fmt::Debug for SendType<ST>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(obj) => write!(f, "Object({obj:?})"),
            Self::RawBuffer(payload) => write!(
                f,
                "RawBuffer(uncompressed_bytes={})",
                payload.shards.uncompressed_size()
            ),
        }
    }
}

pub enum RecvType<RT>
where
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    Object(RT),
    RawBuffer(RawBufferMessage),
}

impl<RT> fmt::Debug for RecvType<RT>
where
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(obj) => write!(f, "Object({obj:?})"),
            Self::RawBuffer(msg) => write!(
                f,
                "RawBuffer(surface={:?}, kind={:?}, bytes={})",
                msg.header.surface,
                msg.header.kind,
                msg.bytes.len()
            ),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum MessageType {
    Object,
    RawBuffer,
}

impl Framed for MessageType {
    fn framed_write<W: Write>(&self, stream: &mut W) -> Result<()> {
        let val: u8 = (*self).into();
        val.framed_write(stream)
    }

    fn framed_read<R: Read>(stream: &mut R) -> Result<Self> {
        Self::try_from(u8::framed_read(stream).location(loc!())?).location(loc!())
    }
}

fn read_loop<R, RT>(mut stream: R, output_channel: channel::SyncSender<RecvType<RT>>) -> Result<()>
where
    R: Read,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    // TODO: try tuning this based on the number of cpus the machine has.
    let mut decompressor =
        ShardingDecompressor::new(NonZeroUsize::new(8).unwrap()).location(loc!())?;

    Version::new().compare_and_warn(&Version::framed_read(&mut stream).location(loc!())?);

    loop {
        let message_type = MessageType::framed_read(&mut stream).location(loc!())?;
        debug!("read message_type: {:?}", message_type);

        // read_exact blocks waiting for data, so start the span afterward.
        let _span = debug_span!("serializer_read_loop").entered();

        match message_type {
            MessageType::Object => {
                CompressedShards::streaming_framed_decompress_with(
                    &mut stream,
                    &mut decompressor,
                    |buf| {
                        let obj = RecvType::Object(
                            debug_span!("deserialize")
                                .in_scope(|| rkyv::from_bytes(buf))
                                .location(loc!())?,
                        );
                        debug!("read obj: {obj:?}");
                        output_channel.send(obj)
                        // The error type is not Send + Sync, which anyhow requires.
                            .map_err(|e| anyhow!("{e}"))
                            .location(loc!())?;
                        Ok(())
                    },
                )
                .location(loc!())?;
            },
            MessageType::RawBuffer => {
                let header = RawBufferHeader::framed_read(&mut stream).location(loc!())?;
                let bytes = CompressedShards::streaming_framed_decompress_to_owned(
                    &mut stream,
                    &mut decompressor,
                )
                .location(loc!())?;
                let obj = RecvType::RawBuffer(RawBufferMessage { header, bytes });
                debug!("read obj: {obj:?}");
                output_channel.send(obj)
                // The error type is not Send + Sync, which anyhow requires.
                    .map_err(|e| anyhow!("{e}"))
                    .location(loc!())?;
            },
        }
    }
}

#[derive(Clone)]
struct OnConnectFrame {
    message_type: MessageType,
    compressed_shards: CompressedShards,
}

fn write_loop<W, ST>(
    stream: W,
    input_channel: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) -> Result<()>
where
    W: Write,
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    let (_, wmem_max) = socket_buffer_limits().location(loc!())?;
    let mut stream = BufWriter::with_capacity(
        wmem_max, // match the socket's buffer size
        stream,
    );

    // This compressor is only used for objects, not raw buffers, so it doesn't
    // need a lot of threads,
    let mut compressor =
        ShardingCompressor::new(NonZeroUsize::new(1).unwrap(), 1).location(loc!())?;

    Version::new().framed_write(&mut stream).location(loc!())?;
    stream.flush().location(loc!())?;

    // Send any requested per-connection messages. This is primarily used by
    // clients to automatically re-handshake after reconnect.
    {
        let frames = on_connect_frames.lock().unwrap().clone();
        for frame in frames {
            frame
                .message_type
                .framed_write(&mut stream)
                .location(loc!())?;
            frame
                .compressed_shards
                .framed_write(&mut stream)
                .location(loc!())?;
        }
        stream.flush().location(loc!())?;
    }

    loop {
        let obj = match input_channel.recv_timeout(Duration::from_secs(1)) {
            Ok(obj) => obj,
            Err(RecvTimeoutError::Timeout) => {
                if !other_end_connected.load(Ordering::Acquire) {
                    break;
                } else {
                    continue;
                }
            },
            Err(RecvTimeoutError::Disconnected) => break,
        };

        match obj {
            SendType::Object(obj) => {
                let _span = debug_span!("serializer_write_loop").entered();
                MessageType::Object.framed_write(&mut stream).location(loc!())?;

                let serialized_data = ArcSlice::new(
                    debug_span!("serialize")
                        .in_scope(|| rkyv::to_bytes::<RancorError>(&obj))
                        .location(loc!())?,
                );

                let shards = compressor
                    .compress(
                        NonZeroUsize::new(16).unwrap(),
                        serialized_data,
                    );
                shards.framed_write(&mut stream).location(loc!())?;
            },
            SendType::RawBuffer(payload) => {
                let _span = debug_span!("serializer_write_loop").entered();
                MessageType::RawBuffer
                    .framed_write(&mut stream)
                    .location(loc!())?;
                let header = RawBufferHeader {
                    version: RawBufferHeader::V2,
                    kind: payload.kind,
                    surface: Some(payload.surface),
                };
                header.framed_write(&mut stream).location(loc!())?;
                payload.shards.framed_write(&mut stream).location(loc!())?;
            },
        }

        stream.flush().location(loc!())?;
    }

    Ok(())
}

fn accept_loop_inner<ST, RT, S>(
    stream: S,
    read_channel_tx: channel::SyncSender<RecvType<RT>>,
    write_channel_rx: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    S: CloneableStream,
{
    thread::scope(|scope| {
        let read_stream = stream.clone_stream().unwrap();
        let write_stream = stream;

        let read_handle = scope.spawn(move || {
            if let Err(err) = read_loop(read_stream, read_channel_tx).location(loc!()) {
                warn!("read_loop failed: {err:?}");
                return;
            }
        });

        let write_handle = scope.spawn(move || {
            if let Err(err) = write_loop(write_stream, write_channel_rx, other_end_connected, on_connect_frames)
                .location(loc!())
            {
                warn!("write_loop failed: {err:?}");
                return;
            }
        });

        utils::join_unwrap(read_handle);
        utils::join_unwrap(write_handle);
    });
}

#[cfg(unix)]
fn accept_loop_unix<ST, RT>(
    listener: UnixListener,
    read_channel_tx: channel::SyncSender<RecvType<RT>>,
    write_channel_rx: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    thread::scope(|_scope| {
        loop {
            debug!("waiting for client connection");
            let (stream, peer) = listener.accept().unwrap();
            info!("wprs client connected from {peer:?}");
            accept_loop_inner(
                stream.try_clone().unwrap(),
                read_channel_tx.clone(),
                write_channel_rx.clone(),
                other_end_connected.clone(),
                on_connect_frames.clone(),
            );
            stream.shutdown_both().unwrap();
        }
    });
}

fn accept_loop_tcp<ST, RT>(
    listener: TcpListener,
    read_channel_tx: channel::SyncSender<RecvType<RT>>,
    write_channel_rx: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    thread::scope(|_scope| {
        loop {
            debug!("waiting for client connection");
            let (stream, peer) = listener.accept().unwrap();
            info!("wprs client connected from {peer:?}");
            accept_loop_inner(
                stream.try_clone().unwrap(),
                read_channel_tx.clone(),
                write_channel_rx.clone(),
                other_end_connected.clone(),
                on_connect_frames.clone(),
            );
            stream.shutdown_both().unwrap();
        }
    });
}

#[cfg(unix)]
fn client_connect_loop_unix<ST, RT>(
    sock_path: PathBuf,
    read_channel_tx: channel::SyncSender<RecvType<RT>>,
    write_channel_rx: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    let mut backoff = Duration::from_millis(100);
    let backoff_max = Duration::from_secs(5);

    loop {
        match UnixStream::connect(&sock_path) {
            Ok(stream) => {
                enlarge_socket_buffer(&stream);
                other_end_connected.store(true, Ordering::Release);
                info!("wprs client connected to {sock_path:?}");

                accept_loop_inner(
                    stream,
                    read_channel_tx.clone(),
                    write_channel_rx.clone(),
                    other_end_connected.clone(),
                    on_connect_frames.clone(),
                );

                // If we disconnected, try again with backoff reset.
                backoff = Duration::from_millis(100);
                info!("server disconnected; reconnecting...");
            },
            Err(err) => {
                other_end_connected.store(false, Ordering::Release);
                warn!(
                    "unable to connect to server at {:?}: {err:?}; retrying in {:?}",
                    sock_path, backoff
                );
                thread::sleep(backoff);
                backoff = backoff.saturating_mul(2).min(backoff_max);
            },
        }
    }
}

fn client_connect_loop_tcp<ST, RT>(
    addr: SocketAddr,
    read_channel_tx: channel::SyncSender<RecvType<RT>>,
    write_channel_rx: Receiver<SendType<ST>>,
    other_end_connected: Arc<AtomicBool>,
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
) where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    let mut backoff = Duration::from_millis(100);
    let backoff_max = Duration::from_secs(5);

    loop {
        match TcpStream::connect(addr) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                #[cfg(unix)]
                enlarge_socket_buffer(&stream);

                other_end_connected.store(true, Ordering::Release);
                info!("wprs client connected to {addr:?}");

                accept_loop_inner(
                    stream,
                    read_channel_tx.clone(),
                    write_channel_rx.clone(),
                    other_end_connected.clone(),
                    on_connect_frames.clone(),
                );

                backoff = Duration::from_millis(100);
                info!("server disconnected; reconnecting...");
            },
            Err(err) => {
                other_end_connected.store(false, Ordering::Release);
                warn!(
                    "unable to connect to server at {addr:?}: {err:?}; retrying in {:?}",
                    backoff
                );
                thread::sleep(backoff);

                backoff = backoff.saturating_mul(2).min(backoff_max);
            },
        }
    }
}

pub struct SerializerClientOptions<ST>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    pub auto_reconnect: bool,
    pub on_connect: Vec<SendType<ST>>,
}

impl<ST> Default for SerializerClientOptions<ST>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    fn default() -> Self {
        Self {
            auto_reconnect: true,
            on_connect: Vec::new(),
        }
    }
}

pub struct Serializer<ST, RT>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    read_handle: Option<Channel<RecvType<RT>>>,
    write_handle: DiscardingSender<Sender<SendType<ST>>>,
    other_end_connected: Arc<AtomicBool>,
    #[allow(dead_code)]
    on_connect_frames: Arc<Mutex<Vec<OnConnectFrame>>>,
    #[allow(dead_code)]
    transport_guard: Option<TransportGuard>,
}

impl<ST, RT> Serializer<ST, RT>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    fn encode_on_connect_frame(msg: SendType<ST>) -> Result<OnConnectFrame> {
        match msg {
            SendType::Object(obj) => {
                let serialized_data = ArcSlice::new(
                    debug_span!("serialize")
                        .in_scope(|| rkyv::to_bytes::<RancorError>(&obj))
                        .location(loc!())?,
                );

                // This is only used for per-connection setup (handshake style)
                // messages; keep compression lightweight.
                let mut compressor =
                    ShardingCompressor::new(NonZeroUsize::new(1).unwrap(), 1).location(loc!())?;
                let shards = compressor.compress(NonZeroUsize::new(1).unwrap(), serialized_data);

                Ok(OnConnectFrame {
                    message_type: MessageType::Object,
                    compressed_shards: shards,
                })
            },
            SendType::RawBuffer(_) => {
                bail!("RawBuffer payloads are not allowed in on_connect frames")
            },
        }
    }

    pub fn new_server_endpoint(endpoint: Endpoint) -> Result<Self> {
        endpoint.warn_if_non_loopback("wprs server endpoint");

        match endpoint {
            Endpoint::Unix { path } => Self::new_server(&path),
            Endpoint::Tcp { addr } => Self::new_server_tcp(addr),
            Endpoint::Ssh { .. } => {
                bail!(
                    "ssh endpoint is only supported for clients (use ssh port forwarding to expose a local tcp/unix endpoint for the server)"
                )
            },
        }
    }

    pub fn new_client_endpoint(endpoint: Endpoint) -> Result<Self> {
        Self::new_client_endpoint_with_options(endpoint, SerializerClientOptions::default())
    }

    pub fn new_client_endpoint_with_options(
        endpoint: Endpoint,
        options: SerializerClientOptions<ST>,
    ) -> Result<Self> {
        let (resolved, guard) = setup_client_transport(endpoint).location(loc!())?;
        resolved.warn_if_non_loopback("wprs client endpoint");

        let mut s = match &resolved {
            Endpoint::Unix { path } => Self::new_client_with_options(path, options),
            Endpoint::Tcp { addr } => Self::new_client_tcp_with_options(*addr, options),
            Endpoint::Ssh { .. } => {
                unreachable!("ssh forwarding resolves to a concrete local endpoint")
            },
        }
        .location(loc!())?;
        s.transport_guard = guard.map(|g| g.into_inner());
        Ok(s)
    }

    pub fn new_server<P: AsRef<Path>>(sock_path: P) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = sock_path;
            bail!("unix socket server is not supported on this platform")
        }

        #[cfg(unix)]
        {
            let listener = utils::bind_user_socket(sock_path).location(loc!())?;
            enlarge_socket_buffer(&listener);

            let (reader_tx, reader_rx): (channel::SyncSender<RecvType<RT>>, Channel<RecvType<RT>>) =
                channel::sync_channel(CHANNEL_SIZE);
            let (writer_tx, writer_rx): (Sender<SendType<ST>>, Receiver<SendType<ST>>) =
                crossbeam_channel::unbounded();
            let other_end_connected = Arc::new(AtomicBool::new(false));
            let on_connect_frames = Arc::new(Mutex::new(Vec::new()));

            {
                let other_end_connected = other_end_connected.clone();
                let on_connect_frames = on_connect_frames.clone();
                thread::spawn(move || {
                    accept_loop_unix(
                        listener,
                        reader_tx,
                        writer_rx,
                        other_end_connected,
                        on_connect_frames,
                    )
                });
            }

            let writer_tx = DiscardingSender {
                sender: writer_tx,
                actually_send: other_end_connected.clone(),
            };

            Ok(Self {
                read_handle: Some(reader_rx),
                write_handle: writer_tx,
                other_end_connected,
                on_connect_frames,
                transport_guard: None,
            })
        }
    }

    pub fn new_client<P: AsRef<Path>>(sock_path: P) -> Result<Self> {
        Self::new_client_with_options(sock_path, SerializerClientOptions::default())
    }

    pub fn new_client_with_options<P: AsRef<Path>>(
        sock_path: P,
        options: SerializerClientOptions<ST>,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = sock_path;
            bail!("unix socket client is not supported on this platform")
        }

        #[cfg(unix)]
        {
            let sock_path = sock_path.as_ref().to_path_buf();

            let (reader_tx, reader_rx): (channel::SyncSender<RecvType<RT>>, Channel<RecvType<RT>>) =
                channel::sync_channel(CHANNEL_SIZE);
            let (writer_tx, writer_rx): (Sender<SendType<ST>>, Receiver<SendType<ST>>) =
                crossbeam_channel::unbounded();
            let other_end_connected = Arc::new(AtomicBool::new(false));
            let on_connect_frames = Arc::new(Mutex::new(Vec::new()));
            {
                let mut frames = on_connect_frames.lock().unwrap();
                for msg in options.on_connect {
                    frames.push(Self::encode_on_connect_frame(msg).location(loc!())?);
                }
            }

            {
                let other_end_connected = other_end_connected.clone();
                let on_connect_frames = on_connect_frames.clone();

                if options.auto_reconnect {
                    thread::spawn(move || {
                        client_connect_loop_unix(
                            sock_path,
                            reader_tx,
                            writer_rx,
                            other_end_connected,
                            on_connect_frames,
                        )
                    });
                } else {
                    let stream = UnixStream::connect(&sock_path).location(loc!())?;
                    enlarge_socket_buffer(&stream);

                    other_end_connected.store(true, Ordering::Release);
                    thread::spawn(move || {
                        accept_loop_inner(
                            stream,
                            reader_tx,
                            writer_rx,
                            other_end_connected,
                            on_connect_frames,
                        );
                        eprintln!("server disconnected");
                        std::process::exit(1);
                    });
                }
            }

            let writer_tx = DiscardingSender {
                sender: writer_tx,
                actually_send: other_end_connected.clone(),
            };

            Ok(Self {
                read_handle: Some(reader_rx),
                write_handle: writer_tx,
                other_end_connected,
                on_connect_frames,
                transport_guard: None,
            })
        }
    }

    pub fn new_server_tcp(addr: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(addr).location(loc!())?;
        #[cfg(unix)]
        enlarge_socket_buffer(&listener);

        let (reader_tx, reader_rx): (channel::SyncSender<RecvType<RT>>, Channel<RecvType<RT>>) =
            channel::sync_channel(CHANNEL_SIZE);
        let (writer_tx, writer_rx): (Sender<SendType<ST>>, Receiver<SendType<ST>>) =
            crossbeam_channel::unbounded();
        let other_end_connected = Arc::new(AtomicBool::new(false));
        let on_connect_frames = Arc::new(Mutex::new(Vec::new()));

        {
            let other_end_connected = other_end_connected.clone();
            let on_connect_frames = on_connect_frames.clone();
            thread::spawn(move || {
                accept_loop_tcp(
                    listener,
                    reader_tx,
                    writer_rx,
                    other_end_connected,
                    on_connect_frames,
                )
            });
        }

        let writer_tx = DiscardingSender {
            sender: writer_tx,
            actually_send: other_end_connected.clone(),
        };

        Ok(Self {
            read_handle: Some(reader_rx),
            write_handle: writer_tx,
            other_end_connected,
            on_connect_frames,
            transport_guard: None,
        })
    }

    pub fn new_client_tcp(addr: SocketAddr) -> Result<Self> {
        Self::new_client_tcp_with_options(addr, SerializerClientOptions::default())
    }

    pub fn new_client_tcp_with_options(
        addr: SocketAddr,
        options: SerializerClientOptions<ST>,
    ) -> Result<Self> {
        let (reader_tx, reader_rx): (channel::SyncSender<RecvType<RT>>, Channel<RecvType<RT>>) =
            channel::sync_channel(CHANNEL_SIZE);
        let (writer_tx, writer_rx): (Sender<SendType<ST>>, Receiver<SendType<ST>>) =
            crossbeam_channel::unbounded();
        let other_end_connected = Arc::new(AtomicBool::new(false));
        let on_connect_frames = Arc::new(Mutex::new(Vec::new()));

        {
            let mut frames = on_connect_frames.lock().unwrap();
            for msg in options.on_connect {
                frames.push(Self::encode_on_connect_frame(msg).location(loc!())?);
            }
        }

        {
            let other_end_connected = other_end_connected.clone();
            let on_connect_frames = on_connect_frames.clone();
            if options.auto_reconnect {
                thread::spawn(move || {
                    client_connect_loop_tcp(
                        addr,
                        reader_tx,
                        writer_rx,
                        other_end_connected,
                        on_connect_frames,
                    )
                });
            } else {
                let stream = TcpStream::connect(addr).location(loc!())?;
                let _ = stream.set_nodelay(true);
                #[cfg(unix)]
                enlarge_socket_buffer(&stream);

                other_end_connected.store(true, Ordering::Release);
                thread::spawn(move || {
                    accept_loop_inner(
                        stream,
                        reader_tx,
                        writer_rx,
                        other_end_connected,
                        on_connect_frames,
                    );
                    eprintln!("server disconnected");
                    std::process::exit(1);
                });
            }
        }

        let writer_tx = DiscardingSender {
            sender: writer_tx,
            actually_send: other_end_connected.clone(),
        };

        Ok(Self {
            read_handle: Some(reader_rx),
            write_handle: writer_tx,
            other_end_connected,
            on_connect_frames,
            transport_guard: None,
        })
    }

    // TODO: https://github.com/rust-lang/rfcs/issues/1215 - Ideally this would
    // return an &mut, but we can't afford to tie up the entire serializer for,
    // well, ever. Change this to return an &mut once rust supports partial
    // borrowing of struct fields.
    // TODO: rename to receiver.
    pub fn reader(&mut self) -> Option<Channel<RecvType<RT>>> {
        self.read_handle.take()
    }

    // TODO: rename to writer.
    pub fn writer(&self) -> InfallibleSender<'_, DiscardingSender<Sender<SendType<ST>>>> {
        InfallibleSender::new(self.write_handle.clone(), self)
    }

    pub fn other_end_connected(&mut self) -> bool {
        self.other_end_connected.load(Ordering::Acquire)
    }

    pub fn set_other_end_connected(&mut self, state: bool) {
        self.other_end_connected.store(state, Ordering::Relaxed);
    }

    /// Adds a message to be sent on every new transport connection.
    ///
    /// Intended for client-side re-handshake after auto-reconnect.
    pub fn add_on_connect_message(&self, msg: SendType<ST>) -> Result<()> {
        let frame = Self::encode_on_connect_frame(msg).location(loc!())?;
        self.on_connect_frames.lock().unwrap().push(frame);
        Ok(())
    }
}

pub fn new_inproc_serializer_pair<ST, RT>() -> Result<(Serializer<ST, RT>, Serializer<RT, ST>)>
where
    ST: Serializable,
    ST::Archived: Deserialize<ST, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    RT: Serializable,
    RT::Archived: Deserialize<RT, HighDeserializer<RancorError>>
        + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
{
    fn spawn_forwarder<T>(
        input: Receiver<SendType<T>>,
        output: channel::SyncSender<RecvType<T>>,
    ) where
        T: Serializable,
        T::Archived: Deserialize<T, HighDeserializer<RancorError>>
            + for<'a> bytecheck::CheckBytes<HighValidator<'a, RancorError>>,
    {
        std::thread::spawn(move || {
            for msg in input.iter() {
                let out = match msg {
                    SendType::Object(obj) => RecvType::Object(obj),
                    SendType::RawBuffer(payload) => {
                        let header = RawBufferHeader {
                            version: RawBufferHeader::V2,
                            kind: payload.kind,
                            surface: Some(payload.surface),
                        };

                        let bytes = match extract_single_uncompressed_shard(payload.shards) {
                            Ok(bytes) => bytes,
                            Err(shards) => match decompress_shards_to_owned(shards) {
                                Ok(bytes) => bytes,
                                Err(err) => {
                                    warn!("inproc raw buffer decompress failed: {err:?}");
                                    continue;
                                },
                            },
                        };
                        RecvType::RawBuffer(RawBufferMessage { header, bytes })
                    }
                };

                if output.send(out).is_err() {
                    break;
                }
            }
        });
    }

    let (a_reader_tx, a_reader_rx): (channel::SyncSender<RecvType<RT>>, Channel<RecvType<RT>>) =
        channel::sync_channel(CHANNEL_SIZE);
    let (b_reader_tx, b_reader_rx): (channel::SyncSender<RecvType<ST>>, Channel<RecvType<ST>>) =
        channel::sync_channel(CHANNEL_SIZE);

    let (a_writer_tx, a_writer_rx): (Sender<SendType<ST>>, Receiver<SendType<ST>>) =
        crossbeam_channel::unbounded();
    let (b_writer_tx, b_writer_rx): (Sender<SendType<RT>>, Receiver<SendType<RT>>) =
        crossbeam_channel::unbounded();

    let a_connected = Arc::new(AtomicBool::new(true));
    let b_connected = Arc::new(AtomicBool::new(true));

    spawn_forwarder::<ST>(a_writer_rx, b_reader_tx);
    spawn_forwarder::<RT>(b_writer_rx, a_reader_tx);

    let a = Serializer {
        read_handle: Some(a_reader_rx),
        write_handle: DiscardingSender {
            sender: a_writer_tx,
            actually_send: a_connected.clone(),
        },
        other_end_connected: a_connected,
        on_connect_frames: Arc::new(Mutex::new(Vec::new())),
        transport_guard: None,
    };

    let b = Serializer {
        read_handle: Some(b_reader_rx),
        write_handle: DiscardingSender {
            sender: b_writer_tx,
            actually_send: b_connected.clone(),
        },
        other_end_connected: b_connected,
        on_connect_frames: Arc::new(Mutex::new(Vec::new())),
        transport_guard: None,
    };

    Ok((a, b))
}
