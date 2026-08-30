# `clm-vdisplay`
**High-Performance Hybrid Virtual Display & VNC/WebRTC Streaming Engine in Rust (Production-Grade)**

`clm-vdisplay` là giải pháp thay thế toàn diện cụm công nghệ di sản `Xvfb` + `Openbox` + `x11vnc` + `websockify` thành một **Single Native Rust Binary** hiệu năng cao, siêu nhẹ (<15MB RAM/display), 60 FPS ổn định, độ trễ Input-to-Photon cực thấp (<10ms), an toàn bộ nhớ và đạt chuẩn Production-Grade.

---

## 🌟 Tính Năng & Tối Ưu Nổi Bật (Production Features)

### 1. Kiến Trúc Hợp Nhất & Khử Trùng Lặp 100% (`RfbProtocolEngine`)
- Toàn bộ luồng xử lý giao thức RFB 3.8 (Handshake, Security, ClientInit, ServerInit, Encodings, Input Routing) được hợp nhất trong [`RfbProtocolEngine`](./src/rfb/engine.rs) dùng chung cho cả **Native TCP Socket** và **WebSocket noVNC**.
- Khử hoàn toàn sự trùng lặp mã nguồn giữa `tcp_server.rs` và `ws_server.rs`.

### 2. Zero-Copy & Song Song Hóa Rayon Scanlines
- [`TileFramebuffer`](./src/display/framebuffer.rs) hỗ trợ đọc trực tiếp slice bộ đệm không qua trung gian.
- Song song hóa việc chuyển đổi định dạng màu (`BGRA32` $\rightarrow$ `RGB24`/`RGBA32`) theo từng dải hàng scanlines với **Rayon**, tối ưu cho màn hình độ phân giải cao 1080p và 1440p.

### 3. An Toàn Concurrency & Zero Deadlock
- **Scoped Guard & Early Drop**: Toàn bộ dữ liệu trong lock guard được trích xuất trong scope hẹp và giải phóng ngay trước khi gọi async `.await`.
- **Unidirectional Event Channel**: Giao tiếp trạng thái vòng đời phiên qua `tokio::sync::mpsc` một chiều, triệt tiêu hoàn toàn rủi ro deadlock 2 chiều giữa Supervisor và Session.

### 4. Prometheus Metrics & Health Probe
- Tích hợp sẵn endpoint HTTP siêu nhẹ:
  - `GET /health`: Trả về trạng thái hoạt động JSON và phiên bản engine.
  - `GET /metrics`: Xuất bản các chỉ số Prometheus chuẩn (`active_connections`, `frames_encoded_total`, `damage_tiles_total`, `bytes_sent_total`, `last_encode_duration_microseconds`).

### 5. Bảo Mật, Fuzz Testing & CI/CD
- Bắt buộc xác thực Token/WSS cho kết nối từ xa.
- Bộ test fuzzing [`tests/fuzz_test.rs`](./tests/fuzz_test.rs) kiểm thử tính bền vững của bộ giải mã gói tin mạng với dữ liệu ngẫu nhiên/đột biến.
- Tự động hóa kiểm tra CI tại `.github/workflows/ci.yml` (`cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`).

---

## 🛠️ Biên Dịch, Kiểm Thử & Benchmark

### Build Binary Release
```bash
cargo build --release
```
Binary được tạo tại `target/release/clm-vdisplay`.

### Chạy Toàn Bộ Test Suite
```bash
cargo test --all-targets --all-features -- --nocapture
```

### Chạy Benchmark Hiệu Năng (Criterion)
```bash
cargo bench
```

### Kiểm Tra Format & Lint
```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

---

## 🚀 Hướng Dẫn Sử Dụng (CLI)

### 1. Khởi chạy Virtual Display Độc Lập
```bash
clm-vdisplay start \
  --display :100 \
  --resolution 1920x1080x24 \
  --mode hybrid \
  --rfb-port 5900 \
  --ws-port 7861 \
  --metrics-port 9100 \
  --token "secret_token_123"
```

### 2. Khởi chạy Supervisor Daemon
```bash
clm-vdisplay daemon \
  --base-vnc-port 5900 \
  --control-socket /tmp/clm-vdisplay.sock \
  --metrics-port 9100
```

---

## 📁 Cấu Trúc Mã Nguồn

```
clm-vdisplay/
├── Cargo.toml                  # Dependencies & Crate configuration
├── LICENSE                     # Dual MIT / Apache-2.0 License
├── README.md                   # Tài liệu hướng dẫn & tổng quan dự án
├── SPEC.md                     # Bản đặc tả kiến trúc kỹ thuật chi tiết
├── .github/workflows/ci.yml     # GitHub Actions CI Workflow
├── benches/
│   └── encoding_bench.rs       # Criterion benchmarks cho 1080p & 1440p
├── src/
│   ├── main.rs                 # CLI Entry point & daemon launcher
│   ├── lib.rs                  # Library root & public API exports
│   ├── config.rs               # Unified AppConfig & CLI definitions
│   ├── metrics.rs              # Prometheus metrics & Health probe server
│   ├── display/                # Hybrid display server core
│   │   ├── mod.rs
│   │   ├── framebuffer.rs      # Tile framebuffer, SIMD diff & Rayon scanlines
│   │   ├── wayland.rs          # Headless Wayland shm compositor integration
│   │   └── x11.rs              # Headless X11 fallback server & WM
│   ├── rfb/                    # RFB 3.8 Protocol Engine
│   │   ├── mod.rs
│   │   ├── engine.rs           # Unified RFB 3.8 engine for TCP & WebSocket
│   │   ├── tcp_server.rs       # Native TCP VNC Server (Port 5900+)
│   │   ├── ws_server.rs        # WebSocket RFB Bridge (noVNC compatible)
│   │   ├── encoder.rs          # Tight / ZRLE / Raw / Pseudo encoders
│   │   └── message.rs          # RFB packet serialization / deserialization
│   ├── input/                  # Input routing & translation
│   │   ├── mod.rs
│   │   ├── mouse.rs            # Mouse movement, buttons & smooth wheel
│   │   ├── keyboard.rs         # KeySym/KeyCode, modifiers & text injection
│   │   └── clipboard.rs        # Bi-directional UTF-8 clipboard sync
│   ├── server/                 # Multi-display supervisor & session
│   │   ├── mod.rs
│   │   ├── session.rs          # DisplaySession lifecycle & unidirectional events
│   │   └── supervisor.rs       # Active displays registry & IPC socket
│   └── streaming/              # Modern streaming backends
│       ├── mod.rs
│       ├── webrtc.rs           # Feature-gated WebRTC pipeline
│       └── cdp_pipe.rs         # CDP Screencast bridge
└── tests/
    ├── concurrency_test.rs     # Regression tests for concurrent clients & disconnects
    ├── framebuffer_test.rs     # Tests for tile damage tracking & conversions
    ├── fuzz_test.rs            # Fuzz testing for message parsing & pixel formats
    ├── input_routing_test.rs   # Tests for mouse, keyboard & clipboard
    ├── metrics_test.rs         # Tests for /health and /metrics endpoints
    ├── rfb_protocol_test.rs    # Tests for RFB packet parsing & encoders
    ├── server_integration_test.rs # End-to-end TCP RFB handshake & streaming test
    ├── supervisor_test.rs      # Supervisor IPC daemon & multi-display test
    └── ws_protocol_test.rs     # End-to-end WebSocket RFB binary streaming test
```
