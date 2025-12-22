use calloop::channel::Event as CalloopChannelEvent;
use calloop::EventLoop as CalloopEventLoop;

use crate::client::backend::ClientBackend;
use crate::prelude::*;
use crate::protocols::wprs as proto;
use crate::protocols::wprs::Serializer;

/// A client backend that consumes WPRS `Request` messages.
///
/// This is intended for simple backends that only need a synchronized stream of
/// `Request` objects. The runner applies `core::client_sync::ClientSync` so that
/// `RawBuffer` frames are paired with `Surface(Commit)` messages before the
/// backend sees them.
pub trait MessageClientBackend: Send {
    fn name(&self) -> &'static str;

    fn handle_message(&mut self, msg: proto::RecvType<proto::Request>) -> Result<()>;
}

pub struct SyncedClientBackend<B> {
    backend: B,
}

impl<B> SyncedClientBackend<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }
}

impl<B> ClientBackend for SyncedClientBackend<B>
where
    B: MessageClientBackend + 'static,
{
    fn name(&self) -> &'static str {
        self.backend.name()
    }

    fn run(self: Box<Self>, mut serializer: Serializer<proto::Event, proto::Request>) -> Result<()> {
        let reader = serializer.reader().location(loc!())?;

        struct State<B> {
            backend: B,
            client_sync: crate::protocols::wprs::core::client_sync::ClientSync,
        }

        let mut loop_: CalloopEventLoop<State<B>> = CalloopEventLoop::try_new().location(loc!())?;
        let mut state = State {
            backend: self.backend,
            client_sync: crate::protocols::wprs::core::client_sync::ClientSync::new(),
        };

        loop_
            .handle()
            .insert_source(reader, move |event, _metadata, state| {
                if let CalloopChannelEvent::Msg(msg) = event {
                    let msg = match state.client_sync.handle_message(msg).location(loc!()) {
                        Ok(Some(msg)) => msg,
                        Ok(None) => return,
                        Err(err) => {
                            warn!("client_sync failed: {err:?}");
                            return;
                        }
                    };

                    state.backend.handle_message(msg).log_and_ignore(loc!());
                }
            })
            .map_err(|e| anyhow!("insert_source(serializer reader) failed: {e:?}"))?;

        // Hold onto the serializer so its transport threads stay alive.
        // Some backends may also need the writer; this runner is intentionally
        // receive-only.
        let _serializer = serializer;
        loop_.run(None, &mut state, |_| {}).location(loc!())
    }
}

