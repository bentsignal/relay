# Third-party software

Relay is MIT licensed. It includes or links the following free software.

| Component | License | Use |
| --- | --- | --- |
| [usbip-win2](https://github.com/vadimgrn/usbip-win2) 0.9.8.0 (unmodified, Microsoft-signed binaries) | BSD-2-Clause | Windows virtual USB host controller that Relay's audio devices plug into |
| [libopus](https://opus-codec.org) | BSD-3-Clause | Network audio codec |
| [nnnoiseless](https://github.com/jneem/nnnoiseless) (RNNoise) | BSD-3-Clause | Noise suppression |
| [GPUI](https://github.com/zed-industries/zed) | Apache-2.0 | User interface |
| [quinn](https://github.com/quinn-rs/quinn), [rustls](https://github.com/rustls/rustls), [ring](https://github.com/briansmith/ring) | MIT / Apache-2.0 / ISC-style | Encrypted LAN transport |
| [cpal](https://github.com/RustAudio/cpal) | Apache-2.0 | Audio device I/O |
| [mdns-sd](https://github.com/keepsimple1/mdns-sd) | Apache-2.0 / MIT | Device discovery |

All other Rust dependencies are MIT and/or Apache-2.0 licensed; see `Cargo.lock`.
The usbip-win2 license text ships next to its files in `usbip/LICENSE-usbip-win2.txt`.
