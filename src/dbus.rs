//! D-Bus control interface for atvvoice.
//!
//! Exposes `org.atvvoice.Daemon` on the session bus, allowing external programs
//! to query status and control the microphone.
//!
//! Requires the `dbus` cargo feature (enabled by default).

use tokio::sync::{mpsc, watch};
use zbus::interface;

use crate::atvv::{ExternalCommand, State};

/// D-Bus object path for the daemon interface.
const DBUS_OBJECT_PATH: &str = "/org/atvvoice/Daemon";

/// Capacity for the external command channel (D-Bus → session).
const COMMAND_CHANNEL_CAPACITY: usize = 16;

/// Static info about the daemon, set at startup.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub(crate) device_address: String,
    pub(crate) node_name: String,
}

/// D-Bus interface implementation.
pub struct DaemonInterface {
    command_tx: mpsc::Sender<ExternalCommand>,
    state_rx: watch::Receiver<State>,
    info: DaemonInfo,
}

impl DaemonInterface {
    pub fn new(
        command_tx: mpsc::Sender<ExternalCommand>,
        state_rx: watch::Receiver<State>,
        info: DaemonInfo,
    ) -> Self {
        Self {
            command_tx,
            state_rx,
            info,
        }
    }
}

impl DaemonInterface {
    async fn send_command(&self, cmd: ExternalCommand) -> zbus::fdo::Result<()> {
        self.command_tx
            .send(cmd)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("send failed: {e}")))?;
        Ok(())
    }
}

#[interface(name = "org.atvvoice.Daemon")]
impl DaemonInterface {
    /// Open the microphone (start streaming).
    async fn mic_open(&self) -> zbus::fdo::Result<()> {
        self.send_command(ExternalCommand::MicOpen).await
    }

    /// Close the microphone (stop streaming).
    async fn mic_close(&self) -> zbus::fdo::Result<()> {
        self.send_command(ExternalCommand::MicClose).await
    }

    /// Toggle the microphone based on current state.
    async fn mic_toggle(&self) -> zbus::fdo::Result<()> {
        self.send_command(ExternalCommand::MicToggle).await
    }

    /// Current state: "disconnected", "connected", "opening", "streaming".
    #[zbus(property)]
    async fn state(&self) -> String {
        self.state_rx.borrow().to_string()
    }

    /// Bluetooth address of the connected remote.
    #[zbus(property)]
    async fn device_address(&self) -> &str {
        &self.info.device_address
    }

    /// PipeWire node name.
    #[zbus(property)]
    async fn node_name(&self) -> &str {
        &self.info.node_name
    }

    /// Emitted when the mic state changes.
    #[zbus(signal)]
    pub async fn mic_state_changed(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        state: &str,
    ) -> zbus::Result<()>;
}

/// Spawn the D-Bus service on the session bus.
/// Returns the command receiver for the ATVV session to consume.
///
/// # Errors
///
/// Returns an error if the D-Bus connection cannot be established or the
/// requested bus name is already owned by another process.
pub async fn serve(
    state_rx: watch::Receiver<State>,
    info: DaemonInfo,
    bus_name: &str,
) -> anyhow::Result<(mpsc::Receiver<ExternalCommand>, zbus::Connection)> {
    let (command_tx, command_rx) = mpsc::channel::<ExternalCommand>(COMMAND_CHANNEL_CAPACITY);
    let iface = DaemonInterface::new(command_tx, state_rx.clone(), info);

    let connection = zbus::connection::Builder::session()?
        .name(bus_name)?
        .serve_at(DBUS_OBJECT_PATH, iface)?
        .build()
        .await?;

    // Spawn a task to emit StateChanged signals when the state changes.
    let conn = connection.clone();
    tokio::spawn(async move {
        let mut rx = state_rx;
        // Track previous state to avoid emitting duplicate D-Bus signals when
        // the watch channel fires but the state value hasn't actually changed.
        let mut prev_state = *rx.borrow();
        while rx.changed().await.is_ok() {
            let new_state = *rx.borrow();
            if new_state != prev_state {
                prev_state = new_state;
                let object_server = conn.object_server();
                match object_server
                    .interface::<_, DaemonInterface>(DBUS_OBJECT_PATH)
                    .await
                {
                    Ok(iface_ref) => {
                        let state_str = new_state.to_string();
                        if let Err(e) = DaemonInterface::mic_state_changed(
                            iface_ref.signal_emitter(),
                            &state_str,
                        )
                        .await
                        {
                            tracing::warn!("failed to emit D-Bus signal: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::debug!("D-Bus interface lookup failed: {e}");
                    }
                }
            }
        }
    });

    tracing::info!("D-Bus interface registered on session bus ({bus_name})");
    Ok((command_rx, connection))
}
