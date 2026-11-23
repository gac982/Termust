# Changelog

Todos los cambios notables de este proyecto se documentan en este archivo.
El formato sigue [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/)
y el versionado es [SemVer](https://semver.org/lang/es/).

## [1.2.0] - 2026-11-23

Release de tipografía y tamaño del terminal, transparencia acotada al fondo de
la consola e historial de comandos como modal interno.

### Added

- Preferencias → Texto: tipografía (familias monoespaciadas instaladas) y
  tamaño (6–32 puntos) del terminal, con vista previa. El zoom sigue
  multiplicando el tamaño elegido.

### Fixed

- La transparencia afecta solo al fondo del terminal: la barra superior y los
  paneles quedan opacos, y el fondo tiene un tope del 80 % para que el texto
  siga siendo legible.
- El historial de comandos se muestra como modal interno centrado sobre la
  ventana (búsqueda, flechas, Enter y Esc) en vez de una ventana flotante; se
  elimina el warning de GDK del popup anterior.

## [1.1.0] - 2026-11-23

Segunda versión estable: columna de archivos con diff de git por archivo, vista
renderizada de Markdown y corrección del prompt duplicado al abrir paneles.

### Added

- Tercera columna de archivos a la derecha del terminal: al abrir un `.md` o un
  archivo de código desde el árbol, el visor se muestra como columna con
  pestañas `Archivo | Diff`. El diff del archivo abierto usa el motor de git
  local (working tree o index) y se recarga al entrar a la pestaña.
- En ventanas angostas (<720 px), abrir un archivo oculta el árbol y abrir el
  árbol cierra la columna, sin cambiar la preferencia guardada de `F9`.
- Vista renderizada de Markdown en la columna de archivos: los `.md` abren como
  documento (títulos, listas, citas, código, tablas, imágenes locales y links
  clickeables), con toggle `Vista | Fuente`; el editor externo sigue disponible.

### Fixed

- El prompt del terminal ya no se duplica al abrir la columna de archivos: los
  cambios de tamaño durante la animación se agrupan en un único resize y el
  shell recibe un solo `SIGWINCH`.

## [1.0.0] - 2026-11-22

Primera versión estable de Termust: terminal para GNOME con pestañas, paneles,
archivos, visor, git, agentes, temas, historial y una base de accesibilidad.

### Added

- Motor VT sobre `alacritty_terminal` con OSC 7/133/633, scrollback y conformidad
  básica; integración bash/zsh sin tocar dotfiles.
- Pestañas horizontales y verticales, restauración de sesión atómica, splits
  hasta 4 paneles, ratios y foco persistidos, confirmación de cierre.
- Panel de archivos lazy con watch, type-ahead, colores/iconos y acciones de
  solo lectura; panel de git local (status, stage/unstage, diff, commit) y
  remotos con push SSH/HTTPS, vault cifrado y keyring del host.
- Visor de solo lectura con tree-sitter para 14 lenguajes, folds, outline,
  búsqueda, límites de tamaño y garantía de no escritura.
- Historial de comandos de 10k/2 MB, importación lazy y popup con búsqueda;
  temas (37 paletas), transparencia, zoom y preferencias Adwaita.
- Detección de agentes `opencode`, `pi` y `agy` con reglas TOML, estados y barra
  de agente; HIG (About/Shortcuts/Overview), accesibilidad básica e i18n es/en.
- Presupuesto de performance medido: hard limits verdes local y Flatpak, GL
  descartado por PSS y cap global de scrollback de 64 MB.

### Security

- Vault SQLite con XChaCha20-Poly1305, AAD por origen, zeroize y clave maestra
  en Secret Service; remotes validados y push sin secretos en argv/env/URL/disco.
- Sin telemetría, sin lectura de `~/.ssh` ni claves; Flatpak con `--filesystem=host`
  documentado y `flatpak-spawn` encapsulado.

### Known limitations

- Cobertura i18n parcial (superficie principal; quedan strings en módulos).
- Orca no verificado end-to-end; screenshots de AppStream pendientes.
- Caché de render/GL no adoptada; Flatpak está por encima de los objetivos de
  PSS/arranque aunque dentro de los hard limits.
- Sin staging por hunk, sin force push, sin edición de archivos.

## [0.1.0] - 2026-11-17

### Added

- Repositorio inicial: licencia GPL-3.0-or-later, README y este changelog.
- Workspace con 15 crates, `app` y harness de conformidad.
- Tests de arquitectura (8 reglas), higiene del repo y consistencia del App ID.
- Ventana vacía con `AdwApplicationWindow`; Cairo por defecto y GL opt-in.
- Flatpak mínimo para GNOME 51 con build offline, `.desktop`, metainfo, gschema e iconos.
