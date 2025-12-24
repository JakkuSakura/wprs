use std::path::PathBuf;
use std::time::Duration;

use wprs::prelude::*;
use wprs::protocols::wprs::serializer::new_inproc_serializer_pair;
use wprs::protocols::wprs::types::Event;
use wprs::protocols::wprs::types::Request;
use wprs::protocols::wprs::transport;
use wprs::server::backend::BackendObservation;
use wprs::server::backend::PollingBackend;
use wprs::server::backend::ServerBackend;
use wprs::server::backend::TickMode;

#[cfg(target_os = "macos")]
use wprs::server::backends::macos::MacosWindowBackend;
#[cfg(target_os = "macos")]
use wprs::server::backends::macos::MacosWindowBackendConfig;

#[cfg(target_os = "macos")]
struct RecorderBackend {
    inner: MacosWindowBackend,
    output_dir: PathBuf,
    frame_seq: std::collections::HashMap<u64, u64>,
    window_idx: std::collections::HashMap<u64, u64>,
    next_idx: u64,
}

#[cfg(target_os = "macos")]
impl RecorderBackend {
    fn new(pid: u32, output_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&output_dir).location(loc!())?;
        Ok(Self {
            inner: MacosWindowBackend::new(MacosWindowBackendConfig {
                dpi: None,
                target_pid: Some(pid),
            }),
            output_dir,
            frame_seq: std::collections::HashMap::new(),
            window_idx: std::collections::HashMap::new(),
            next_idx: 0,
        })
    }

    fn handle_observations(&mut self, observations: Vec<BackendObservation>) -> Result<()> {
        for observation in observations {
            match observation {
                BackendObservation::SurfaceCommit { surface, frame } => {
                    let Some(frame) = frame else {
                        continue;
                    };

                    let width = u32::try_from(frame.metadata.width)
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or_else(|| {
                            Error::InvalidArgument(format!(
                                "invalid width {}",
                                frame.metadata.width
                            ))
                        })?;
                    let height = u32::try_from(frame.metadata.height)
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or_else(|| {
                            Error::InvalidArgument(format!(
                                "invalid height {}",
                                frame.metadata.height
                            ))
                        })?;
                    let stride = usize::try_from(frame.metadata.stride)
                        .ok()
                        .filter(|v| *v > 0)
                        .ok_or_else(|| {
                            Error::InvalidArgument(format!(
                                "invalid stride {}",
                                frame.metadata.stride
                            ))
                        })?;

                    let png =
                        transport::encode_png_from_bgra(width, height, stride, &frame.bgra)
                            .location(loc!())?;
                    let idx = self
                        .window_idx
                        .entry(surface.id.0)
                        .or_insert_with(|| {
                            let idx = self.next_idx;
                            self.next_idx += 1;
                            idx
                        });
                    let seq = self.frame_seq.entry(surface.id.0).or_insert(0);
                    let filename = format!("window_{}_{}.png", idx, *seq);
                    *seq += 1;
                    let path = self.output_dir.join(filename);
                    std::fs::write(&path, png).location(loc!())?;
                }
                BackendObservation::SurfaceDestroyed { surface, .. } => {
                    self.frame_seq.remove(&surface.0);
                    self.window_idx.remove(&surface.0);
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl ServerBackend for RecorderBackend {
    fn tick_mode(&self) -> TickMode {
        TickMode::Polling
    }

    fn run(
        mut self: Box<Self>,
        _serializer: wprs::protocols::wprs::serializer::Serializer<Request, Event>,
        tick_interval: Option<Duration>,
    ) -> Result<()> {
        let tick_interval = tick_interval.unwrap_or(Duration::from_secs(1));

        let snapshot = self.inner.initial_snapshot().location(loc!())?;
        self.handle_observations(snapshot).location(loc!())?;

        loop {
            let observations = self.inner.poll().location(loc!())?;
            self.handle_observations(observations).location(loc!())?;
            std::thread::sleep(tick_interval);
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: record_pid_windows <pid> [output_dir]\n\nExample:\n  record_pid_windows 1234 target/record/pid_1234"
        );
        return Ok(());
    }

    let pid: u32 = args[1]
        .parse()
        .map_err(|_| Error::InvalidArgument("pid must be a u32".to_string()))?;
    let output_dir = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("target/record/pid_{pid}")));
    let tick_interval = Duration::from_secs(1);

    #[cfg(target_os = "macos")]
    {
        let backend = RecorderBackend::new(pid, output_dir).location(loc!())?;
        let (serializer, _client) = new_inproc_serializer_pair::<Request, Event>().location(loc!())?;
        Box::new(backend)
            .run(serializer, Some(tick_interval))
            .location(loc!())
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, output_dir, tick_interval);
        bail!(Error::Unsupported(
            "record_pid_windows is only supported on macOS".to_string(),
        ))
    }
}
