//! BLE device discovery and ATVV GATT characteristic resolution.

use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};
use bluer::{gatt::{remote::Characteristic, WriteOp}, Adapter, AdapterEvent, Address, Device, Uuid};
use futures::{Stream, StreamExt};

use crate::atvv::{BleDevice, BleFut, BleStream, DeviceConnectionEvent};

/// Wrap a `CharacteristicReader` (from `AcquireNotify`) into a `Stream<Item = Vec<u8>>`.
fn reader_to_stream(
    reader: bluer::gatt::CharacteristicReader,
) -> Pin<Box<dyn Stream<Item = Vec<u8>> + Send>> {
    Box::pin(futures::stream::unfold(reader, |reader| async move {
        match reader.recv().await {
            Ok(data) => Some((data, reader)),
            Err(e) => {
                tracing::debug!("BLE notification stream ended: {e}");
                None
            }
        }
    }))
}

/// ATVV Service UUID: AB5E0001-5A21-4F05-BC7D-AF01F617B664
pub const ATVV_SERVICE: Uuid = Uuid::from_u128(0xab5e0001_5a21_4f05_bc7d_af01f617b664);

/// ATVV TX Characteristic (Host → Remote): AB5E0002
pub const ATVV_CHAR_TX: Uuid = Uuid::from_u128(0xab5e0002_5a21_4f05_bc7d_af01f617b664);

/// ATVV RX Characteristic (Remote → Host, audio): AB5E0003
pub const ATVV_CHAR_RX: Uuid = Uuid::from_u128(0xab5e0003_5a21_4f05_bc7d_af01f617b664);

/// ATVV CTL Characteristic (Remote → Host, control): AB5E0004
pub const ATVV_CHAR_CTL: Uuid = Uuid::from_u128(0xab5e0004_5a21_4f05_bc7d_af01f617b664);

/// Philips vendor write characteristic UUID (ff01) — vendor activation channel.
/// Writing "ntf_enable" here signals the remote to enter ATVV-ready mode.
/// Full UUID as observed on device: 02f00000-0000-0000-0000-00000000ff01
const PHILIPS_VENDOR_FF01: Uuid = Uuid::from_u128(0x02f00000_0000_0000_0000_00000000ff01);

/// Philips vendor notify characteristic UUID (ff02) — must be subscribed before writing ff01.
/// Full UUID as observed on device: 02f00000-0000-0000-0000-00000000ff02
const PHILIPS_VENDOR_FF02: Uuid = Uuid::from_u128(0x02f00000_0000_0000_0000_00000000ff02);

/// Resolved ATVV characteristics for a connected device.
pub struct AtvvChars {
    pub tx: Characteristic,
    pub rx: Characteristic,
    pub ctl: Characteristic,
}

impl std::fmt::Debug for AtvvChars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtvvChars")
            .field("tx", &"<Characteristic>")
            .field("rx", &"<Characteristic>")
            .field("ctl", &"<Characteristic>")
            .finish()
    }
}

/// Real BLE device implementation wrapping bluer types.
/// Borrows the device and characteristics so main.rs retains ownership
/// for reconnect logic and shutdown MIC_CLOSE.
pub struct BluerDevice<'a> {
    pub device: &'a Device,
    pub chars: &'a AtvvChars,
}

impl BleDevice for BluerDevice<'_> {
    fn write_command(&self, data: &[u8]) -> BleFut<'_, ()> {
        let data = data.to_vec();
        Box::pin(async move {
            // TX characteristic has the 'write' flag (not 'write-without-response'),
            // so we must use ATT Write Request (WriteOp::Request) instead of the
            // default WriteOp::Command (ATT Write Command). Remotes silently ignore
            // Write Commands on characteristics that only support Write Request.
            self.chars.tx.write_ext(&data, &bluer::gatt::remote::CharacteristicWriteRequest {
                op_type: WriteOp::Request,
                ..Default::default()
            }).await?;
            Ok(())
        })
    }

    fn ctl_notifications(&self) -> BleFut<'_, BleStream<Vec<u8>>> {
        Box::pin(async {
            let reader = self.chars.ctl.notify_io().await
                .context("Failed to acquire exclusive CTL notifications. \
                          Another ATVVoice instance may be connected to this device.")?;
            tracing::debug!("CTL AcquireNotify: exclusive access, MTU={}", reader.mtu());
            Ok(reader_to_stream(reader))
        })
    }

    fn rx_notifications(&self) -> BleFut<'_, BleStream<Vec<u8>>> {
        Box::pin(async {
            let reader = self.chars.rx.notify_io().await
                .context("Failed to acquire exclusive RX notifications. \
                          Another ATVVoice instance may be connected to this device.")?;
            tracing::debug!("RX AcquireNotify: exclusive access, MTU={}", reader.mtu());
            Ok(reader_to_stream(reader))
        })
    }

    fn connection_events(&self) -> BleFut<'_, BleStream<DeviceConnectionEvent>> {
        Box::pin(async {
            let stream = self.device.events().await?;
            let mapped = stream.filter_map(|event| async move {
                if let bluer::DeviceEvent::PropertyChanged(
                    bluer::DeviceProperty::Connected(false),
                ) = event
                {
                    Some(DeviceConnectionEvent::Disconnected)
                } else {
                    None
                }
            });
            Ok(
                Box::pin(mapped) as BleStream<DeviceConnectionEvent>,
            )
        })
    }
}

/// Returns `true` if the given address should be skipped during discovery.
fn should_skip(addr: Address, filter_addr: Option<Address>, excluded: &[Address]) -> bool {
    if excluded.contains(&addr) {
        return true;
    }
    if let Some(filter) = filter_addr {
        if addr != filter {
            return true;
        }
    }
    false
}

/// Find a bonded device that advertises the ATVV service.
/// If `filter_addr` is Some, only match that specific address.
/// Addresses in `exclude` are skipped (e.g. devices locked by another instance).
pub async fn find_atvv_device(
    adapter: &Adapter,
    filter_addr: Option<Address>,
    exclude: &[Address],
) -> Result<Device> {
    // First check already-known devices
    for addr in adapter.device_addresses().await? {
        if should_skip(addr, filter_addr, exclude) {
            continue;
        }
        let device = adapter.device(addr)?;
        if let Ok(Some(uuids)) = device.uuids().await {
            if uuids.contains(&ATVV_SERVICE) {
                tracing::info!(
                    "Found ATVV device: {} ({})",
                    device.name().await.ok().flatten().unwrap_or_default(),
                    addr
                );
                return Ok(device);
            }
        }
    }

    // Fall back to discovery stream
    tracing::info!("Scanning for ATVV devices...");
    let discover = adapter.discover_devices().await?;
    tokio::pin!(discover);
    while let Some(evt) = discover.next().await {
        if let AdapterEvent::DeviceAdded(addr) = evt {
            if should_skip(addr, filter_addr, exclude) {
                continue;
            }
            let device = adapter.device(addr)?;
            if let Ok(Some(uuids)) = device.uuids().await {
                if uuids.contains(&ATVV_SERVICE) {
                    tracing::info!("Discovered ATVV device: {}", addr);
                    return Ok(device);
                }
            }
        }
    }

    anyhow::bail!("BLE discovery stream ended without finding an ATVV device (adapter may have been removed)")
}

/// Resolve the three ATVV GATT characteristics from a connected device.
pub async fn resolve_chars(device: &Device) -> Result<AtvvChars> {
    let mut tx = None;
    let mut rx = None;
    let mut ctl = None;

    for service in device.services().await? {
        if service.uuid().await? != ATVV_SERVICE {
            continue;
        }

        for char in service.characteristics().await? {
            match char.uuid().await? {
                uuid if uuid == ATVV_CHAR_TX => tx = Some(char),
                uuid if uuid == ATVV_CHAR_RX => rx = Some(char),
                uuid if uuid == ATVV_CHAR_CTL => ctl = Some(char),
                _ => {}
            }
        }
        break; // Found the ATVV service; no need to check other services.
    }

    Ok(AtvvChars {
        tx: tx.context("ATVV TX characteristic not found")?,
        rx: rx.context("ATVV RX characteristic not found")?,
        ctl: ctl.context("ATVV CTL characteristic not found")?,
    })
}

/// Attempt Philips vendor handshake if the device requires it.
///
/// Some Philips TV Voice remotes (e.g. URMT26RST004) require a proprietary
/// vendor handshake before they will respond to ATVV protocol commands.
///
/// The handshake sequence (reverse-engineered from device traffic):
///  1. Subscribe to ff02 notifications (remote sends "ntf_enable" as a greeting)
///  2. Write "ntf_enable" to ff01 (signals readiness to the remote)
///  3. Wait ~500ms for remote to enter ATVV-ready state
///
/// After this sequence, the remote accepts ATT subscriptions on ATVV characteristics
/// and responds to GET_CAPS commands.
///
/// Returns `Ok(true)` if the handshake was performed, `Ok(false)` if not a Philips device.
pub async fn vendor_handshake(device: &Device) -> Result<Option<crate::atvv::BleStream<Vec<u8>>>> {
    let mut ff01_char = None;
    let mut ff02_char = None;

    for service in device.services().await? {
        if service.uuid().await? == ATVV_SERVICE {
            continue;
        }
        for char in service.characteristics().await? {
            match char.uuid().await? {
                uuid if uuid == PHILIPS_VENDOR_FF01 => ff01_char = Some(char),
                uuid if uuid == PHILIPS_VENDOR_FF02 => ff02_char = Some(char),
                _ => {}
            }
        }
        if ff01_char.is_some() {
            break;
        }
    }

    let ff01 = match ff01_char {
        Some(c) => c,
        None => return Ok(None), // Not a Philips device
    };

    tracing::info!("Detected Philips vendor service. Performing vendor handshake...");

    // Step 1: Subscribe to ff02 (required before remote accepts ff01 write).
    // The remote sends "ntf_enable" on ff02 as a greeting when subscribed.
    // We return this stream to the caller so it stays subscribed for the session —
    // dropping it (StopNotify) resets the remote's ATVV-ready state.
    let ff02_stream: Option<crate::atvv::BleStream<Vec<u8>>> = if let Some(ff02) = ff02_char {
        match ff02.notify().await {
            Ok(stream) => {
                tracing::debug!("Subscribed to vendor ff02 notifications");
                Some(Box::pin(stream))
            }
            Err(e) => {
                tracing::warn!("Vendor ff02 StartNotify failed (continuing): {e}");
                None
            }
        }
    } else {
        tracing::warn!("Vendor ff02 not found; handshake may be incomplete");
        None
    };

    // Step 2: Write "ntf_enable" to ff01 to signal readiness.
    ff01.write_ext(b"ntf_enable", &bluer::gatt::remote::CharacteristicWriteRequest {
        op_type: WriteOp::Request,
        ..Default::default()
    }).await.context("Philips vendor handshake: write to ff01 failed")?;
    tracing::debug!("Vendor handshake: wrote ntf_enable to ff01");

    // Step 3: Wait for remote to process and enter ATVV-ready state.
    tokio::time::sleep(Duration::from_millis(500)).await;

    tracing::info!("Vendor handshake complete");
    // Return the ff02 stream so the caller can hold it alive for the session.
    // Dropping it would call StopNotify and reset the remote's ATVV-ready state.
    Ok(ff02_stream)
}
