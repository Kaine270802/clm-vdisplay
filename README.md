# `clm-vdisplay`

Rust engine for a **headed X11 virtual display** plus **RFB 3.8** (native TCP and WebSocket). CloakBrowser Gateway on Hugging Face Space spawns one process per browser session instead of `x11vnc` + websockify.

This README matches the tree at `b9dbb4371e22d6dec5ee85a11c808315e1f604c9` (and later commits on `main`). Marketing numbers that are not measured here are omitted.

---

## What it does

- Supervises **Xvfb** (`--manage-x11`) with MIT-SHM + XDamage capture.
- Speaks RFB 3.8 to noVNC / Tight clients: **Tight (zlib lossless)** → ZRLE → RAW.
- Injects **mouse and keyboard** into X11 with **XTest** (must finish attaching before RFB input; fire-and-forget injector was a past bug).
- Caps capture with `--fps` (CLI default **60**; **Gateway Space passes `--fps 15`**).
- Optional Prometheus `--metrics-port` (`/health`, `/metrics`). Gateway spawn **does not** pass `--metrics-port`.

## What it does not do (yet)

- **WebRTC**: Cargo feature `webrtc` is an empty stub. Do not enable for product.
- **X11 CLIPBOARD bridge**: `ClipboardManager` stores RFB `ClientCutText` / `ServerCutText` in RAM. Chrome in Xvfb pastes from the X11 clipboard, so OS copy/paste through noVNC is **not** equivalent to a real shared clipboard. Gateway UI still uses a Paste Text workaround that types keys.
- **Live FPS change**: `--fps` is applied when the capture loop starts. There is no SetFps on the supervisor socket.
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

IPC actions today: Create / Stop / List / Get / InjectText / SetClipboard. **No SetFps.** Gateway does **not** run this daemon; it `start`s one binary per session.

---

## Layout

```
src/
  config.rs          # clap: start --fps, --manage-x11, …
  display/           # framebuffer, x11 session glue
  rfb/               # engine, encoder (Tight/ZRLE/RAW), tcp + ws servers
  input/             # mouse, keyboard, in-memory clipboard
  x11/               # capture, XTest injector, Xvfb supervisor
  server/            # DisplaySession, optional daemon supervisor
  streaming/         # webrtc stub, cdp_pipe
```

---

## Pin from CloakBrowser Gateway

`hf-browser/space/Dockerfile` clones `https://github.com/Kaine270802/clm-vdisplay.git` and `git checkout ${CLM_VDISPLAY_GIT_REF}`. Change engine behavior on this repo, push `main`, then bump that ARG on `clmbrowser` and rebuild the Space.

License: MIT OR Apache-2.0.
