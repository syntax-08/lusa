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

The v1.3.0 body is [`.github/release-notes/v1.3.0.md`](../.github/release-notes/v1.3.0.md).
It follows this structure and documents typed volatile MMIO access, packed
structs, explicit alignment, and the native `~=` fix.
