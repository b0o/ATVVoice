use anyhow::{Context, Result};
use bluer::{gatt::remote::Characteristic, Adapter, AdapterEvent, Address, Device, Uuid};
use futures::StreamExt;

/// ATVV Service UUID: AB5E0001-5A21-4F05-BC7D-AF01F617B664
pub const ATVV_SERVICE: Uuid = Uuid::from_u128(0xab5e0001_5a21_4f05_bc7d_af01f617b664);

/// ATVV TX Characteristic (Host → Remote): AB5E0002
pub const ATVV_CHAR_TX: Uuid = Uuid::from_u128(0xab5e0002_5a21_4f05_bc7d_af01f617b664);

/// ATVV RX Characteristic (Remote → Host, audio): AB5E0003
pub const ATVV_CHAR_RX: Uuid = Uuid::from_u128(0xab5e0003_5a21_4f05_bc7d_af01f617b664);

/// ATVV CTL Characteristic (Remote → Host, control): AB5E0004
pub const ATVV_CHAR_CTL: Uuid = Uuid::from_u128(0xab5e0004_5a21_4f05_bc7d_af01f617b664);

/// Resolved ATVV characteristics for a connected device.
pub struct AtvvChars {
    pub tx: Characteristic,
    pub rx: Characteristic,
    pub ctl: Characteristic,
}

/// Find a bonded device that advertises the ATVV service.
/// If `filter_addr` is Some, only match that specific address.
pub async fn find_atvv_device(
    adapter: &Adapter,
    filter_addr: Option<Address>,
) -> Result<Device> {
    // First check already-known devices
    for addr in adapter.device_addresses().await? {
        if let Some(filter) = filter_addr {
            if addr != filter {
                continue;
            }
        }
        let device = adapter.device(addr)?;
        if let Ok(Some(uuids)) = device.uuids().await {
            if uuids.contains(&ATVV_SERVICE) {
                tracing::info!(
                    "Found ATVV device: {} ({})",
                    device.name().await?.unwrap_or_default(),
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
            if let Some(filter) = filter_addr {
                if addr != filter {
                    continue;
                }
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

    anyhow::bail!("No ATVV device found")
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
    }

    Ok(AtvvChars {
        tx: tx.context("ATVV TX characteristic not found")?,
        rx: rx.context("ATVV RX characteristic not found")?,
        ctl: ctl.context("ATVV CTL characteristic not found")?,
    })
}
