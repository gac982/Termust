# Termust

Terminal para GNOME, escrito en Rust con GTK4 y libadwaita. Pensado para uso
personal diario: pestañas, paneles divididos, panel de archivos, visor de
código de solo lectura, git integrado y detección de agentes de código.

**Estado:** M1 — walking skeleton. Workspace, tests de arquitectura, CI local, ventana vacía y Flatpak mínimo funcionando. Todavía no hay terminal usable (eso llega en M2).

## Requisitos de desarrollo

- Linux con GNOME y Wayland (X11 como fallback).
- Rust 1.100 (`rust-toolchain.toml`); MSRV 1.92. El Flatpak usa el Rust del SDK (1.98).
- GTK 4.22 y libadwaita 1.9.
- Flatpak y flatpak-builder para el empaquetado.

## Compilar y probar

```sh
cargo build
cargo test --workspace
./scripts/ci/verify.sh
```

La app abre una ventana vacía con `cargo run -p termust-app`.

## Empaquetado

```sh
./scripts/flatpak-src.sh
flatpak-builder --user --install --force-clean build-dir flatpak/io.github.termust.Termust.yml
flatpak run io.github.termust.Termust
```

## Licencia

GPL-3.0-or-later. Ver `LICENSE`.
