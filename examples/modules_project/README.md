# Modular firmware project (Module 23)

A multi-file Lusa project that demonstrates namespaced modules, `use` aliases,
`pub use` re-exports, and default-private items.

```text
main.lusa                  entry point
board.lusa                 file module: pins, private helper, inline `timing`
sensors/mod.lusa           file module: re-exports both reading types
sensors/temperature.lusa   file module: private decode + public sample API
sensors/pressure.lusa      file module: pressure readings
protocol/mod.lusa          file module: re-exports the frame API
protocol/frame.lusa        file module: private checksum + public make/encode
```

Run it with any engine:

```powershell
lusalang run examples/modules_project/main.lusa
lusalang run --vm examples/modules_project/main.lusa
lusalang run --native examples/modules_project/main.lusa
lusalang build examples/modules_project/main.lusa
```

Expected output:

```text
150
40
32 true
true
101 true
1 32 34
322
```

Notes:

- Module files resolve relative to `main.lusa`: `mod board;` loads `board.lusa`,
  while `mod sensors;` loads `sensors/mod.lusa`.
- Items are private by default. `board::label`, `temperature::decode`, and
  `frame::checksum` are only callable inside their own module.
- `sensors::Temperature`, `sensors::Pressure`, and the `protocol` facade are
  `pub use` re-exports, so callers do not depend on the internal file layout.
- `board::timing` is an inline module nested inside a file module.
- Prefer top-level assignments (`pub TICKS_PER_MS = 8`) for constants read from
  functions; the C11 backend keeps top-level `local` bindings inside `main`.
