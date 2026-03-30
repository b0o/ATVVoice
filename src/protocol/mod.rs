pub mod types;
pub mod v04;
pub mod v10;

use anyhow::Result;
use types::*;

/// Protocol abstraction for ATVV v0.4 and v1.0.
///
/// Implementations encode commands, parse CTL notifications, negotiate codecs,
/// and decode audio frames according to their spec version.
pub trait Protocol: Send {
    /// Protocol version this implementation speaks.
    fn version(&self) -> ProtocolVersion;

    /// Build MIC_OPEN command bytes.
    fn mic_open_cmd(&self) -> Vec<u8>;

    /// Build MIC_CLOSE command bytes for a given stream.
    fn mic_close_cmd(&self, stream_id: StreamId) -> Vec<u8>;

    /// Build keepalive command bytes.
    /// v1.0: MIC_EXTEND. v0.4: MIC_OPEN (no MIC_EXTEND support).
    fn keepalive_cmd(&self, stream_id: StreamId) -> Vec<u8>;

    /// Parse a CTL notification into a typed event.
    fn parse_ctl(&self, data: &[u8]) -> CtlEvent;

    /// Process received capabilities. Returns the negotiated codec.
    fn on_caps_resp(&mut self, caps: &Capabilities) -> Result<Codec>;

    /// Decode an audio frame notification into PCM samples.
    /// Returns None if the frame is invalid/wrong size.
    fn decode_audio(&mut self, data: &[u8]) -> Option<AudioFrame>;

    /// Handle AUDIO_SYNC (update internal decoder state).
    fn on_audio_sync(&mut self, sync: &AudioSyncData);
}

/// Negotiate the best codec from the intersection of remote and our support.
/// Prefers highest quality (16kHz > 8kHz).
pub(crate) fn negotiate_codec(remote_codecs: types::Codecs) -> Result<types::Codec> {
    let ours = types::Codecs::ADPCM_8KHZ | types::Codecs::ADPCM_16KHZ;
    let common = remote_codecs & ours;
    if common.contains(types::Codecs::ADPCM_16KHZ) {
        Ok(types::Codec::Adpcm16kHz)
    } else if common.contains(types::Codecs::ADPCM_8KHZ) {
        Ok(types::Codec::Adpcm8kHz)
    } else {
        anyhow::bail!(
            "no common codec: remote supports {:?}, we support {:?}",
            remote_codecs,
            ours
        )
    }
}

/// PTT | HTT support bitmask for GET_CAPS (OnRequest always implied).
const SUPPORTED_INTERACTION_MODELS: u8 = 0x03;

/// Build a GET_CAPS command advertising our maximum supported version (v1.0).
#[must_use]
pub fn get_caps_cmd() -> Vec<u8> {
    let ver = types::ProtocolVersion::V1_0.wire_value().to_be_bytes();
    let codecs = (types::Codecs::ADPCM_8KHZ | types::Codecs::ADPCM_16KHZ).bits();
    vec![
        u8::from(types::TxOpcode::GetCaps),
        ver[0],
        ver[1],
        0x00, // reserved
        codecs,
        SUPPORTED_INTERACTION_MODELS,
    ]
}

/// Parse a CAPS_RESP notification from raw CTL bytes.
/// Returns None for invalid, unrecognized, or too-short data.
pub fn parse_caps_resp(data: &[u8]) -> Option<types::Capabilities> {
    if data.len() < 3 {
        return None;
    }
    let opcode = types::CtlOpcode::try_from(data[0]).ok()?;
    if opcode != types::CtlOpcode::CapsResp {
        return None;
    }
    let version_wire = u16::from_be_bytes([data[1], data[2]]);
    let version = types::ProtocolVersion::from_wire(version_wire)?;
    match version {
        types::ProtocolVersion::V0_4 => v04::parse_caps_resp_payload(data),
        types::ProtocolVersion::V1_0 => v10::parse_caps_resp_payload(data),
    }
}

/// Create a Protocol already initialized with negotiated capabilities.
///
/// Returns the protocol implementation and the negotiated codec. The codec is
/// returned here because `on_caps_resp` already performs negotiation internally,
/// avoiding the need for callers to call `negotiate_codec` separately.
pub fn create_protocol(caps: &Capabilities) -> Result<(Box<dyn Protocol>, Codec)> {
    let mut p: Box<dyn Protocol> = match caps.version {
        ProtocolVersion::V0_4 => Box::new(v04::ProtocolV04::new()),
        ProtocolVersion::V1_0 => Box::new(v10::ProtocolV10::new()),
    };
    let codec = p.on_caps_resp(caps)?;
    Ok((p, codec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_caps_cmd_is_v1_0() {
        let cmd = get_caps_cmd();
        assert_eq!(cmd[0], u8::from(types::TxOpcode::GetCaps));
        // Version: 0x0100 (v1.0 big-endian)
        assert_eq!(cmd[1], 0x01);
        assert_eq!(cmd[2], 0x00);
        // Reserved
        assert_eq!(cmd[3], 0x00);
        // Codecs: ADPCM_8KHZ | ADPCM_16KHZ = 0x03
        assert_eq!(cmd[4], 0x03);
        // Interaction models: PTT | HTT = 0x03
        assert_eq!(cmd[5], 0x03);
        assert_eq!(cmd.len(), 6);
    }

    #[test]
    fn test_parse_caps_resp_v04() {
        let data: &[u8] = &[0x0B, 0x00, 0x04, 0x00, 0x01, 0x00, 0x86, 0x00, 0x14];
        let caps = parse_caps_resp(data).unwrap();
        assert_eq!(caps.version, types::ProtocolVersion::V0_4);
        assert_eq!(caps.codecs, types::Codecs::ADPCM_8KHZ);
    }

    #[test]
    fn test_parse_caps_resp_v10() {
        let data: &[u8] = &[0x0B, 0x01, 0x00, 0x03, 0x02, 0x00, 0x80];
        let caps = parse_caps_resp(data).unwrap();
        assert_eq!(caps.version, types::ProtocolVersion::V1_0);
        assert_eq!(
            caps.codecs,
            types::Codecs::ADPCM_8KHZ | types::Codecs::ADPCM_16KHZ
        );
    }

    #[test]
    fn test_parse_caps_resp_unknown_version() {
        // Version 0x0200 -- unknown
        let data: &[u8] = &[0x0B, 0x02, 0x00, 0x03, 0x02, 0x00, 0x80];
        assert!(parse_caps_resp(data).is_none());
    }

    #[test]
    fn test_parse_caps_resp_too_short() {
        assert!(parse_caps_resp(&[0x0B, 0x00]).is_none());
    }

    #[test]
    fn test_parse_caps_resp_wrong_opcode() {
        let data: &[u8] = &[0x04, 0x00, 0x04, 0x00, 0x01, 0x00, 0x86, 0x00, 0x14];
        assert!(parse_caps_resp(data).is_none());
    }

    #[test]
    fn test_create_protocol_v04() {
        let caps = types::Capabilities {
            version: types::ProtocolVersion::V0_4,
            codecs: types::Codecs::ADPCM_8KHZ,
            interaction_model: types::InteractionModel::OnRequest,
            audio_frame_size: types::AudioFrameSize(134),
        };
        let (p, codec) = create_protocol(&caps).unwrap();
        assert_eq!(p.version(), types::ProtocolVersion::V0_4);
        assert_eq!(codec, types::Codec::Adpcm8kHz);
    }

    #[test]
    fn test_create_protocol_v10() {
        let caps = types::Capabilities {
            version: types::ProtocolVersion::V1_0,
            codecs: types::Codecs::ADPCM_8KHZ | types::Codecs::ADPCM_16KHZ,
            interaction_model: types::InteractionModel::HoldToTalk,
            audio_frame_size: types::AudioFrameSize(128),
        };
        let (p, codec) = create_protocol(&caps).unwrap();
        assert_eq!(p.version(), types::ProtocolVersion::V1_0);
        assert_eq!(codec, types::Codec::Adpcm16kHz);
    }

    // ── negotiate_codec tests ───────────────────────────────────────

    #[test]
    fn test_negotiate_codec_prefers_16khz() {
        let remote = types::Codecs::ADPCM_8KHZ | types::Codecs::ADPCM_16KHZ;
        let codec = negotiate_codec(remote).unwrap();
        assert_eq!(codec, types::Codec::Adpcm16kHz);
    }

    #[test]
    fn test_negotiate_codec_falls_back_to_8khz() {
        let remote = types::Codecs::ADPCM_8KHZ;
        let codec = negotiate_codec(remote).unwrap();
        assert_eq!(codec, types::Codec::Adpcm8kHz);
    }

    #[test]
    fn test_negotiate_codec_no_common_codec() {
        let remote = types::Codecs::empty();
        assert!(negotiate_codec(remote).is_err());
    }
}
