# Release notes policy

From v0.5.0 onward, every GitHub Release body is written for users, not only as
a file changelog. The release workflow reads the body from
`.github/release-notes/<tag>.md` and validates that it contains the required
sections before publishing.

## Required structure

1. **Título da versão** — short, human-readable.
2. **Introdução de Impacto** — why this release exists, what development pain
   it removes, and who benefits.
3. **Mapeamento de Features** — a concise list of the main user-facing changes.
4. **Como Usar** — a functional code block plus the exact commands needed to
   try the feature immediately.
5. **Compatibilidade / Limites** — what still requires a C compiler, which
   platforms are supported, and any known constraints.
6. **Instalação** — how to use the attached `lusa-setup.zip` or platform
   tarballs.

## Workflow contract

- File path: `.github/release-notes/${GITHUB_REF_NAME}.md`.
- The publish job fails if the file is missing, empty, or does not contain the
  headings `## Introdução de Impacto`, `## Mapeamento de Features`, and
  `## Como Usar`.
- `generate_release_notes` is disabled: release notes are curated and
  educational, not an auto-generated commit list.
- Examples in the notes must be copy-paste runnable with the release binaries.
  If a feature is compile-only, say so explicitly.

## Template

````markdown
# Lusa vX.Y.Z — short value statement

## Introdução de Impacto
Why this release matters and the pain it removes.

## Mapeamento de Features
- Feature one.
- Feature two.

## Como Usar
```lusa
-- runnable example
```

```sh
lusalang run example.lusa
lusalang build example.lusa
```

## Compatibilidade
Known limits and toolchain requirements.

## Instalação
Download `lusa-setup.zip` or the platform tarball.
````
