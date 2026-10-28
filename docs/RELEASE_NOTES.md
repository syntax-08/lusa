# Release notes policy

From v0.5.0 onward, every GitHub Release body is written in English for users,
not only as a file changelog. The release workflow reads the body from
`.github/release-notes/<tag>.md` and validates that it contains the required
sections before publishing.

## Required structure

1. **Version title** — short, human-readable.
2. **Impact Introduction** — why this release exists, what development pain it
   removes, and who benefits.
3. **Feature Highlights** — a concise list of the main user-facing changes.
4. **How to Use** — a functional code block plus the exact commands needed to
   try the feature immediately.
5. **Compatibility / Limits** — what still requires a C compiler, which
   platforms are supported, and any known constraints.
6. **Installation** — how to use the attached `lusa-setup.zip` or platform
   tarballs.

## Workflow contract

- File path: `.github/release-notes/${GITHUB_REF_NAME}.md`.
- The publish job fails if the file is missing, empty, or does not contain the
  headings `## Impact Introduction`, `## Feature Highlights`, and
  `## How to Use`.
- `generate_release_notes` is disabled: release notes are curated and
  educational, not an auto-generated commit list.
- Examples in the notes must be copy-paste runnable with the release binaries.
  If a feature is compile-only, say so explicitly.
- All release notes, workflow messages, documentation updates, code comments,
  compiler diagnostics, feature names, and changelogs must be written in
  English. Do not add localized variants to the repository.

## Template

````markdown
# Lusa vX.Y.Z — short value statement

## Impact Introduction
Why this release matters and the pain it removes.

## Feature Highlights
- Feature one.
- Feature two.

## How to Use
```lusa
-- runnable example
```

```sh
lusalang run example.lusa
lusalang build example.lusa
```

## Compatibility
Known limits and toolchain requirements.

## Installation
Download `lusa-setup.zip` or the platform tarball.
````

## Current release

The v2.1.0 body is [`.github/release-notes/v2.1.0.md`](../.github/release-notes/v2.1.0.md).
It follows this structure and documents `--target rp2040`/`stm32f4`/`esp32c3`,
per-board startup and linker maps, Renode platforms with GPIO, timers/SysTick,
and UART, the three volatile-MMIO examples, and the module 25
cross-build/emulation suite.

The v2.0.0 body remains at
[`.github/release-notes/v2.0.0.md`](../.github/release-notes/v2.0.0.md) and
documents `lusalang build --target`, freestanding RISC-V
`riscv32-virt`/`riscv64-virt` images, embedded startup and linker assets,
Renode platform/UART capture, and the hardware-emulation test suite.

The previous v1.9.0 body remains at
[`.github/release-notes/v1.9.0.md`](../.github/release-notes/v1.9.0.md) and
documents namespaced modules (`mod name;` and `mod name { ... }`), `use`
aliases and globs, `pub`/`pub use` visibility, `E0350` privacy diagnostics, and
multi-file projects shared by the interpreter, bytecode VM, and C11 backend.

The v1.8.0 body remains at
[`.github/release-notes/v1.8.0.md`](../.github/release-notes/v1.8.0.md) and
documents `async function`/`async fn`, lazy `task<T>` handles,
`await`/`.await`/`::await`/`task.await`, the
`task.spawn`/`run`/`yield_now`/`is_done`/`status` executor surface, and the
freestanding static-arena executor.

The v1.7.0 body remains at
[`.github/release-notes/v1.7.0.md`](../.github/release-notes/v1.7.0.md) and
documents built-in `Result<T, E>`/`Option<T>`, constructors, combinators,
constructor `match` patterns, and the `?` operator.

Every verified release must also publish a GitHub Release (with the annotated
tag and curated notes), not only push the commit and tag to `origin/main`.
