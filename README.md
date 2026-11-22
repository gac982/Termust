# Termust

Terminal para GNOME escrito en Rust con GTK4 y libadwaita. Pensado para uso
diario: pestañas y paneles, archivos, visor de código, git local/remoto,
historial de comandos, temas y detección de agentes.

**Estado:** 1.0.0. Ver `CHANGELOG.md` y `doc/user/` para uso y limitaciones.

## Funciones

- Terminal VT con bash/zsh, OSC 7/133/633, colores 256/truecolor y scrollback.
- Pestañas horizontales o verticales, splits hasta 4 paneles y sesión restaurable.
- Panel de archivos lazy, visor de solo lectura con tree-sitter (14 lenguajes,
  folds, outline, búsqueda).
- Git local (status/stage/unstage/diff/commit) y remotos (SSH/HTTPS) con vault
  cifrado; nunca force push.
- Historial de comandos, temas (37 paletas), transparencia, zoom y preferencias.
- Agentes `opencode`, `pi` y `agy`: nombre, tarea y estado.
- i18n es/en en la superficie principal; accesibilidad básica y HIG.

## Requisitos

- Linux con GNOME/Wayland (X11 fallback), GTK 4.22 y libadwaita 1.9.
- Rust 1.100 local (MSRV 1.92) y Flatpak para empaquetar.

## Compilar y probar

```sh
cargo build --release -p termust-app
cargo test --workspace
./scripts/ci/verify.sh
```

## Instalar (Flatpak)

```sh
./scripts/flatpak-src.sh
flatpak-builder --user --install --force-clean build-dir flatpak/io.github.termust.Termost.yml
flatpak run io.github.termust.Termost
```

Para generar un repo local y un bundle:

```sh
./scripts/release.sh
flatpak remote-add --user --if-not-exists --no-gpg-verify termust-local "$PWD/dist/repo"
flatpak install --user termust-local io.github.termust.Termost
```

El repo de producción debe firmarse con `GPG_KEY` (ver `scripts/release.sh`).

## Seguridad y privacidad

- Sin telemetría. El shell corre en el host; el Flatpak usa `--filesystem=host`
  para archivos/git y `flatpak-spawn` encapsulado en `host-exec`.
- Credenciales HTTPS cifradas en SQLite; clave maestra en Secret Service.
- SSH usa el `git`/`ssh` del host y nunca lee claves privadas.

## Licencia

GPL-3.0-or-later. Ver `LICENSE` y `NOTICE`.
