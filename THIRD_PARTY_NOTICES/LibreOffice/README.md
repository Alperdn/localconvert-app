# LibreOffice third-party notices (placeholder)

LocalConvert bundles a pinned LibreOffice build as its Office conversion
engine (see `docs/OFFICE_ENGINE.md`). LibreOffice is distributed under the
Mozilla Public License 2.0, and its distribution bundles numerous
third-party components under their own licenses (fonts, codecs, ICU,
Python, etc.).

`scripts/prepare-office-engine.ps1` copies the pinned installer's own
`LICENSE`, `NOTICE`, `readme`/`readmes`, and any `credits`/`third-party`
license files it ships into this directory verbatim, alongside the
manifest's `license_notice_path` pointer, so the exact notices shipped in
a given build always match the exact LibreOffice build in that installer.

This placeholder exists only so the directory structure and the end-user
"where do I find third-party notices" path are correct before the payload
has been prepared on a build machine. It intentionally contains no
license text of its own - populating it with the real files requires
running the prep script against the actual pinned installer.

**Legal note:** an institutional/legal review of the bundled third-party
notices is still required before final MEB distribution. Do not treat
this placeholder, or the prep script copying files verbatim, as legal
clearance.
