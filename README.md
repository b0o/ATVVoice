# atvvoice

Linux daemon that captures voice audio from BLE TV remotes using the [Google Voice over BLE (ATVV)](https://blog.csdn.net/Weichen_Huang/article/details/109251338) protocol and exposes it as a PipeWire virtual microphone.

### Supported devices

| Device | Status |
|--------|--------|
| G20S Pro | Verified working |
| UR02 | Should work, untested |
| Other ATVV-compatible remotes | Unknown |

If you have a remote you'd like to test, open an issue with the device name, Bluetooth address, and output of `atvvoice -d <ADDR> -vv`. See [docs/research/report.md](docs/research/report.md) for protocol details.

## Requirements

- Linux with BlueZ and PipeWire
- A bonded ATVV-compatible BLE remote (pair with `bluetoothctl`)

## Installation

### Nix flake

```nix
# flake.nix
inputs.atvvoice.url = "github:b0o/atvvoice";
```

**Home Manager module:**

```nix
imports = [ inputs.atvvoice.homeManagerModules.atvvoice ];

services.atvvoice = {
  enable = true;
  device = "AA:BB:CC:DD:EE:FF";  # your remote's BT address
};
```

**As overlay:**

```nix
nixpkgs.overlays = [ inputs.atvvoice.overlays.default ];
# then: pkgs.atvvoice
```

### Pre-built binary

Download from [GitHub Releases](https://github.com/b0o/atvvoice/releases):

```
curl -Lo atvvoice https://github.com/b0o/atvvoice/releases/latest/download/atvvoice-x86_64-linux
chmod +x atvvoice
sudo mv atvvoice /usr/local/bin/
```

Replace `x86_64-linux` with `aarch64-linux` for ARM64.

Requires `libpipewire` and `libdbus` at runtime.

### Cargo

```
cargo install --path .
```

Requires `pipewire` and `dbus` development libraries and `pkg-config`.

## Usage

```
atvvoice -d <BT_ADDRESS> [OPTIONS]
```

| Flag | Default | Description |
|------|---------|-------------|
| `-d, --device` | auto | Bluetooth address filter |
| `-a, --adapter` | auto | BlueZ adapter name |
| `-g, --gain` | 20 | Audio gain in dB |
| `-m, --mode` | toggle | `toggle` (press on/off) or `hold` (hold to stream)\* |
| `--frame-timeout` | 5 | Seconds without frames before auto-closing mic (device asleep) |
| `-t, --idle-timeout` | 0 | Seconds since last button press before auto-closing mic |
| `-v` | off | Verbosity (`-v` debug, `-vv` trace) |

\*Not all remotes support hold-to-stream. The G20S Pro sends a button press event on both press and release, so it only works in toggle mode.

Example:

```
atvvoice -d AA:BB:CC:DD:EE:FF -v --idle-timeout 300
```

The remote appears as "BLE Voice Remote" in PipeWire/PulseAudio audio input settings.

## Home Manager options

All CLI flags have corresponding module options:

```nix
services.atvvoice = {
  enable = true;
  device = "AA:BB:CC:DD:EE:FF";
  mode = "toggle";           # or "hold"
  gain = 20;
  frameTimeout = 5;
  idleTimeout = 300;
  verbose = 1;               # 0-3
};
```

## How it works

```
BLE Remote --[GATT/ATVV]--> atvvoice --[PipeWire]--> Apps
```

1. Connects to the remote via BlueZ D-Bus
2. Subscribes to ATVV GATT notifications (audio + control)
3. On mic button press: sends MIC_OPEN, receives IMA/DVI ADPCM audio frames
4. Decodes ADPCM, applies click removal + lowpass + gain
5. Outputs 8kHz 16-bit mono PCM to a PipeWire virtual source

See [docs/research/report.md](docs/research/report.md) for the full protocol reverse-engineering writeup and [docs/specs/2026-03-23-atvvoice-design.md](docs/specs/2026-03-23-atvvoice-design.md) for the design spec.

## License

MIT
