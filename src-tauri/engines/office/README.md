# Office Engine resource directory (build-time populated)

This directory is the contract `resolver::bundled_path` /
`engines::office_manifest` read from at runtime, resolved relative to the
installed application's own executable
(`<install dir>/engines/office/...`).

Only this `README.md` and `manifest.json.template` are checked into git.
Everything else here is produced by `scripts/prepare-office-engine.ps1`
and is gitignored (see repo-root `.gitignore`) - never commit an actual
LibreOffice payload to source control.

Expected layout after running the prep script:

```
engines/office/
  manifest.json              <- generated from manifest.json.template,
                                 with sha256/bundled_at_build filled in
  LibreOffice/
    program/
      soffice.exe
      soffice.bin
      ... (all files LibreOffice's own installer places under program/)
    share/
      ... (filters, gallery, registry, autocorr, etc. - required for
           Writer/Calc/Impress -> PDF export filters to load)
```

See `docs/OFFICE_ENGINE.md` for the pinned version, source, hash policy,
and exactly which subdirectories were verified as required for
DOCX/XLSX/PPTX/ODT/ODS/ODP -> PDF.
