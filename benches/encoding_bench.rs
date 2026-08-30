use bytes::BytesMut;
use clm_vdisplay::display::framebuffer::{PixelFormat, Rect, TileFramebuffer};
use clm_vdisplay::rfb::encoder::*;
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_framebuffer_1080p_damage(c: &mut Criterion) {
    let mut fb = TileFramebuffer::new(1920, 1080);
    // Initial scan
    let _ = fb.detect_damage_tiles();

    c.bench_function("damage_tracking_1080p_clean", |b| {
        b.iter(|| {
            let rects = fb.detect_damage_tiles();
            black_box(rects);
        })
    });

    let patch = vec![0xAB; 200 * 200 * 4];
    c.bench_function("damage_tracking_1080p_modified", |b| {
        b.iter(|| {
            fb.update_rect(100, 100, 200, 200, &patch, 200 * 4);
            let rects = fb.detect_damage_tiles();
            black_box(rects);
        })
    });
}

fn bench_color_conversion_1080p_rayon(c: &mut Criterion) {
    let fb = TileFramebuffer::new(1920, 1080);
    let rect = Rect::new(0, 0, 1920, 1080);
    let rgb24_fmt = PixelFormat::rgb24();
    let mut out = vec![0u8; 1920 * 1080 * 3];

    c.bench_function("color_conversion_bgra_to_rgb24_1080p_rayon", |b| {
        b.iter(|| {
            fb.extract_rect_bytes_into(&rect, &rgb24_fmt, &mut out);
            black_box(&out[..10]);
        })
    });
}

fn bench_color_conversion_1440p_rayon(c: &mut Criterion) {
    let fb = TileFramebuffer::new(2560, 1440);
    let rect = Rect::new(0, 0, 2560, 1440);
    let rgb24_fmt = PixelFormat::rgb24();
    let mut out = vec![0u8; 2560 * 1440 * 3];

    c.bench_function("color_conversion_bgra_to_rgb24_1440p_rayon", |b| {
        b.iter(|| {
            fb.extract_rect_bytes_into(&rect, &rgb24_fmt, &mut out);
            black_box(&out[..10]);
        })
    });
}

fn bench_tight_encoder_1080p(c: &mut Criterion) {
    let fb = TileFramebuffer::new(1920, 1080);
    let rect = Rect::new(0, 0, 1920, 1080);
    let format = PixelFormat::bgra32();
    let mut buf = BytesMut::with_capacity(1920 * 1080 * 4);

    c.bench_function("tight_encode_1080p_solid", |b| {
        b.iter(|| {
            buf.clear();
            encode_tight_rect(&fb, &rect, &format, &mut buf);
            black_box(buf.len());
        })
    });
}

criterion_group!(
    benches,
    bench_framebuffer_1080p_damage,
    bench_color_conversion_1080p_rayon,
    bench_color_conversion_1440p_rayon,
    bench_tight_encoder_1080p
);
criterion_main!(benches);
