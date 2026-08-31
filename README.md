# `clm-vdisplay`

Rust engine for a **headed X11 virtual display** plus **RFB 3.8** (native TCP and WebSocket). CloakBrowser Gateway on Hugging Face Space spawns one process per browser session instead of `x11vnc` + websockify.

This README matches the behavior of this tree. Marketing numbers that are not measured here are omitted.

---

## What it does

- Supervises **Xvfb** (`--manage-x11`) with MIT-SHM + XDamage capture.
- Speaks RFB 3.8 to noVNC / Tight clients: **Tight (zlib lossless)** → ZRLE → RAW.
- Injects **mouse and keyboard** into X11 with **XTest** (must finish attaching before RFB input; fire-and-forget injector was a past bug).
- Caps capture with `--fps` (CLI default **60**; **Gateway Space passes `--fps 15`**). Live change: RFB client message **SetFps** (type **254**, 4 bytes: `u8 type`, `u8 pad=0`, `u16 fps` big-endian) on the open `/vnc/{browserId}` socket of the **`start` process**. Capture loop reads an `AtomicU32` each wait (min 1). Stored live value is clamped to **1..=30** (slider range). Display (`vnc.html`) sends type 254; do not restart RFB.
- Two-way **text** clipboard: RFB `ClientCutText` → X11 **CLIPBOARD** and **PRIMARY** (XFixes / ICCCM owner) so Chrome Ctrl+V works; XFixes `SelectionNotify` on CLIPBOARD → `ServerCutText`. Applied text is capped at **256 KiB** (truncate + log; RFB session stays up). Parse ceiling 10 MiB is unchanged. Images/files are not supported.
- Optional Prometheus `--metrics-port` (`/health`, `/metrics`). Gateway spawn **does not** pass `--metrics-port`.

## What it does not do (yet)

- **WebRTC**: Cargo feature `webrtc` is an empty stub. Do not enable for product.
- **Supervisor Unix socket as the Space FPS path**: Gateway does **not** run `daemon`. Optional `SupervisorCommand::SetFps` exists for tests only. Live FPS is RFB SetFps on `start`.
- **Proven &lt;15 MB RSS / &lt;10 ms photon**: not measured on cpu-basic Space. Headed Chrome on the same display dominates RAM (~250 MiB parent); VNC encode did not move cgroup memory in Gateway tests.

---

## Build and test

```bash
cargo build --release
cargo test --all-targets --all-features -- --nocapture
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

Binary: `target/release/clm-vdisplay`.

---

## CLI

### Standalone display (what Space uses)

```bash
clm-vdisplay start \
  --display :100 \
  --resolution 1920x1080x24 \
  --mode hybrid \
  --rfb-port 5900 \
  --fps 15 \
  --manage-x11
```

`--ws-port` is optional. Gateway RFB for noVNC is the **TCP** port proxied as `WS /vnc/{browserId}`; it does not pass `--ws-port`.

### Supervisor daemon

```bash
clm-vdisplay daemon \
  --base-vnc-port 5900 \
  --control-socket /tmp/clm-vdisplay.sock
```

IPC actions: Create / Stop / List / Get / InjectText / SetClipboard / SetFps (tests). **Gateway does not run this daemon**; it `start`s one binary per session. Live FPS for Space is RFB type 254, not this socket.

---

## Layout

```
src/
  config.rs          # clap: start --fps, --manage-x11, …
  display/           # framebuffer, x11 session glue
  rfb/               # engine, encoder (Tight/ZRLE/RAW), tcp + ws servers
  input/             # mouse, keyboard, in-memory clipboard (256 KiB cap)
  x11/               # capture, XTest injector, XFixes CLIPBOARD, Xvfb supervisor
  server/            # DisplaySession, optional daemon supervisor
  streaming/         # webrtc stub, cdp_pipe
```

---

## Pin from CloakBrowser Gateway

`hf-browser/space/Dockerfile` clones `https://github.com/Kaine270802/clm-vdisplay.git` and `git checkout ${CLM_VDISPLAY_GIT_REF}`. Change engine behavior on this repo, push `main`, then bump that ARG on `clmbrowser` and rebuild the Space.

License: MIT OR Apache-2.0.
