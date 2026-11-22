//! Renderiza una captura de bytes de terminal a PNG para comparar el renderer.
//!
//! Uso: cargo run -p termust-render-cairo --example render_capture -- <raw> <png> [cols] [rows]

use cairo::{Context, Format, ImageSurface};
use termust_core::terminal::{Size, TerminalEngine};
use termust_render_cairo::CairoRenderer;
use termust_term_engine::AlacrittyEngine;
use termust_text_render::{FontSpec, PaintOpts, TextRenderer};

fn main() {
    let mut args = std::env::args().skip(1);
    let raw = args.next().expect("raw capture path");
    let out = args.next().expect("png output path");
    let cols: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(132);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    let family = args.next().unwrap_or_else(|| "monospace".to_string());
    let size: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12.0);

    let bytes = std::fs::read(&raw).expect("read capture");
    let mut engine = AlacrittyEngine::new(Size::new(cols, rows), 0);
    engine.feed(&bytes);
    let snapshot = engine.snapshot();

    let renderer = CairoRenderer::new();
    let font = FontSpec { family, size };

    let probe = ImageSurface::create(Format::ARgb32, 8, 8).unwrap();
    let probe_ctx = Context::new(&probe).unwrap();
    let metrics = renderer.metrics(&probe_ctx, &font);
    let width = (cols as f64 * metrics.width).ceil() as i32;
    let height = (rows as f64 * metrics.height).ceil() as i32;
    println!(
        "size={:?} cell={}x{} px={}x{} modes={:?} cursor={:?}",
        snapshot.size,
        metrics.width,
        metrics.height,
        width,
        height,
        snapshot.modes,
        snapshot.cursor
    );

    for row in 0..rows {
        let text = snapshot.row_text(row);
        if !text.trim().is_empty() {
            println!("{row:02}: {text:.70}");
        }
    }

    let mut surface = ImageSurface::create(Format::ARgb32, width, height).unwrap();
    let ctx = Context::new(&surface).unwrap();
    renderer.paint(
        &ctx,
        &snapshot,
        &termust_text_render::Theme::default(),
        &PaintOpts {
            font,
            selection: None,
            bg_alpha: 1.0,
        },
    );
    drop(ctx);
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().unwrap();
    let mut ppm = Vec::with_capacity(width as usize * height as usize * 3 + 32);
    ppm.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for y in 0..height as usize {
        let row = &data[y * stride..y * stride + width as usize * 4];
        for px in row.as_chunks::<4>().0 {
            ppm.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    std::fs::write(&out, ppm).unwrap();
    println!("wrote {out}");
}
