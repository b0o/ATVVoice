//! PipeWire virtual audio source.
//!
//! Runs on a dedicated OS thread (not tokio-compatible). Receives decoded PCM
//! frames via [`std::sync::mpsc`] and pushes them to PipeWire as a virtual
//! microphone source.

use std::io;
use std::sync::mpsc;

use pipewire::keys;
use pipewire::main_loop::MainLoop;
use pipewire::properties::properties;
use pipewire::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pipewire::spa::param::ParamType;
use pipewire::spa::pod::serialize::PodSerializer;
use pipewire::spa::pod::{Object, Pod, Value};
use pipewire::spa::utils::{Direction, SpaTypes};
use pipewire::stream::{Stream, StreamFlags};

/// Sample rate for ATVV audio (8 kHz mono).
const SAMPLE_RATE: u32 = 8000;

/// Number of audio channels (mono).
const CHANNELS: u32 = 1;

/// Size of one sample in bytes (i16 = 2 bytes).
const SAMPLE_SIZE: usize = std::mem::size_of::<i16>();

/// Signal to shut down the PipeWire source cleanly.
pub struct Shutdown;

/// Run the PipeWire audio source on the current thread (blocking).
///
/// Reads decoded PCM frames from `audio_rx` and pushes them to PipeWire as a
/// virtual microphone. Returns when `shutdown_rx` receives a [`Shutdown`] message.
///
/// The caller is responsible for sending `Shutdown` when the session ends.
/// This ensures the PipeWire stream is disconnected while the mainloop is still
/// alive, so the node disappears cleanly from audio settings.
///
/// Call from a dedicated `std::thread::spawn`.
pub fn run_pw_source(
    audio_rx: mpsc::Receiver<Vec<i16>>,
    gain_db: f32,
    node_name: &str,
    node_description: &str,
    shutdown_rx: pipewire::channel::Receiver<Shutdown>,
) -> Result<(), pipewire::Error> {
    pipewire::init();

    let mainloop = MainLoop::new(None)?;
    let context = pipewire::context::Context::new(&mainloop)?;
    let core = context.connect(None)?;

    let stream = Stream::new(
        &core,
        node_name,
        properties! {
            *keys::MEDIA_TYPE => "Audio",
            *keys::MEDIA_CATEGORY => "Capture",
            *keys::MEDIA_CLASS => "Audio/Source",
            *keys::MEDIA_ROLE => "Communication",
            *keys::NODE_NAME => node_name,
            *keys::NODE_DESCRIPTION => node_description,
        },
    )?;

    // Buffer of pending PCM samples not yet consumed by PipeWire callbacks.
    let pending: std::cell::RefCell<Vec<i16>> = std::cell::RefCell::new(Vec::new());

    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(|_, _, old, new| {
            tracing::debug!("PipeWire stream state: {old:?} -> {new:?}");
        })
        .process(move |stream, _| {
            {
                let mut buf = pending.borrow_mut();
                loop {
                    match audio_rx.try_recv() {
                        Ok(mut frame) => {
                            crate::adpcm::apply_gain(&mut frame, gain_db);
                            buf.extend_from_slice(&frame);
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        // Channel disconnected — just stop draining.
                        // The Shutdown message will arrive shortly to do orderly teardown.
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }
            }

            let Some(mut pw_buf) = stream.dequeue_buffer() else {
                return;
            };

            let requested = pw_buf.requested() as usize;

            let Some(pw_data) = pw_buf.datas_mut().first_mut() else {
                return;
            };

            let Some(slice) = pw_data.data() else {
                return;
            };

            let buf_capacity = slice.len() / SAMPLE_SIZE;
            let max_samples = if requested > 0 {
                buf_capacity.min(requested)
            } else {
                buf_capacity
            };

            let mut buf = pending.borrow_mut();
            let available = buf.len().min(max_samples);

            for (i, &sample) in buf.iter().take(available).enumerate() {
                let bytes = sample.to_le_bytes();
                let offset = i * SAMPLE_SIZE;
                slice[offset] = bytes[0];
                slice[offset + 1] = bytes[1];
            }

            let silence_start = available * SAMPLE_SIZE;
            let silence_end = max_samples * SAMPLE_SIZE;
            for byte in &mut slice[silence_start..silence_end] {
                *byte = 0;
            }

            if available > 0 {
                buf.drain(..available);
            }

            let chunk = pw_data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = SAMPLE_SIZE as i32;
            *chunk.size_mut() = (max_samples * SAMPLE_SIZE) as u32;
        })
        .register()?;

    // Build the SPA audio format pod for format negotiation.
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::S16LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS);

    let mut position = [0u32; 64];
    position[0] = pipewire::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    audio_info.set_position(position);

    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = PodSerializer::serialize(
        io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .expect("failed to serialize audio format pod")
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).expect("invalid pod bytes")];

    stream.connect(
        Direction::Output,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    tracing::info!("PipeWire source running (8kHz S16LE mono, gain={gain_db}dB)");

    // Attach shutdown receiver to the mainloop event sources.
    // When Shutdown arrives, disconnect the stream (removes the PW node from
    // the audio graph) and quit the mainloop — all while the loop is still
    // running. This follows the pattern used by RustAudio/cpal.
    let mainloop_weak = mainloop.downgrade();
    let _shutdown = shutdown_rx.attach(mainloop.loop_(), move |_: Shutdown| {
        tracing::info!("PipeWire source shutting down");
        let _ = stream.disconnect();
        if let Some(ml) = mainloop_weak.upgrade() {
            ml.quit();
        }
    });

    // Blocks until quit is called from the shutdown handler.
    mainloop.run();

    // After mainloop.run() returns, stream.disconnect() was already called
    // inside the shutdown handler while the loop was alive. Drop order:
    // listener, shutdown handler, core, context, mainloop.
    drop(_listener);
    drop(_shutdown);
    drop(core);
    drop(context);

    tracing::info!("PipeWire source stopped");
    unsafe { pipewire::deinit() };

    Ok(())
}
