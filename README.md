# Termust

Terminal para GNOME, escrito en Rust con GTK4 y libadwaita. Pensado para uso
personal diario: pestañas, paneles divididos, panel de archivos, visor de
código de solo lectura, git integrado y detección de agentes de código.

**Estado:** M0 — investigación y diseño. Todavía no hay código de aplicación.

## Requisitos de desarrollo

- Linux con GNOME y Wayland (X11 como fallback).
- Rust estable (la versión mínima se fija en `rust-toolchain.toml` durante M1).
- GTK 4 y libadwaita (la versión objetivo se fija en M0).
- Flatpak y flatpak-builder para el empaquetado.

## Compilar y probar

El workspace se crea en M1. Hasta entonces no hay binario. Cuando exista:

```sh
cargo build
cargo test
./scripts/ci/verify.sh
```

## Empaquetado

```sh
flatpak-builder --repo=build/repo --force-clean build/dir flatpak/io.github.termust.Termust.yml
flatpak build-bundle build/repo termust.flatpak io.github.termust.Termust
```

## Licencia

GPL-3.0-or-later. Ver `LICENSE`.
