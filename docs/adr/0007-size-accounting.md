# ADR 0007 — Base-image size accounting

**Status:** accepted (M0)

* The 108,000,000-byte ceiling applies to the **installed read-only base**:
  every file on the system volume rounded up to whole filesystem clusters,
  plus one cluster per directory. Raw payload bytes are reported too.
* FAT metadata, unused partition capacity, the sparse disk image, user data,
  caches, swap and optional packages (browser, Tor Browser) are measured and
  reported separately and do not count toward the ceiling.
* No compression is used; the measured files are the files that execute.
* `cargo xtask build` regenerates `docs/size-report.md` and
  `target/coco/size-report.json` on every release build and **fails** when
  the base exceeds the ceiling.
