# LibreOffice third-party notices

MEB-Dönüştür bundles a pinned LibreOffice build as its Office conversion
engine (see `docs/OFFICE_ENGINE.md`). LibreOffice is distributed under the
Mozilla Public License 2.0, and its distribution bundles numerous
third-party components under their own licenses (fonts, codecs, ICU,
Python, etc.).

This directory contains the LibreOffice license, notice, and credits files
shipped with the product (`LICENSE.html`, `license.txt`, `NOTICE`,
`CREDITS.fodt`), copied verbatim from the pinned LibreOffice installer.
`scripts/prepare-office-engine.ps1` performs this copy (including any
`readme`/`readmes` and `third-party` license files the installer ships),
alongside the manifest's `license_notice_path` pointer, so the notices
shipped in a given build match the exact LibreOffice build in that
installer. This directory is the end-user location for third-party
notices of the bundled Office engine.

**Legal note:** an institutional/legal review of the bundled third-party
notices is still required before final MEB distribution. Do not treat
the presence of these files, or the prep script copying files verbatim, as
legal clearance.
