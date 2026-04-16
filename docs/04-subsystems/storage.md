# Storage

## What it does

The storage module provides a unified object storage abstraction built on
the `object_store` crate. It supports local filesystem storage and S3-compatible
cloud storage from multiple vendors. Used by the callrecord module for CDR
and recording uploads, and by other modules for any file persistence needs.

## Key types & entry points

- **`Storage`** — main storage client wrapping an `ObjectStore` with prefix handling and local/remote awareness. Methods: `write()`, `read()`, `delete()`, `list()`, `upload_file()`, `local_path()`. `src/storage/mod.rs`
- **`StorageConfig`** (enum) — `Local { path }` or `S3 { vendor, bucket, region, access_key, secret_key, endpoint, prefix }`. `src/storage/mod.rs`
- **`S3Vendor`** (enum) — AWS, GCP, Azure, Aliyun, Tencent, Minio, DigitalOcean. `src/storage/mod.rs`

## Sub-modules

None — single file module.

## Configuration

Storage is configured indirectly through `[callrecord]` S3 settings or
via `storage_dir` for local file storage. The `StorageConfig` type is
used programmatically by other modules.

## Public API surface

The storage module does not expose HTTP routes. It is a library used by
callrecord, sipflow, and addon modules.

## See also

- [callrecord.md](callrecord.md) — Primary consumer for CDR and recording uploads
- [upload-retry.md](upload-retry.md) — Retry scheduler that uses Storage for re-uploads

---
**Status:** ✅ Shipped
**Source:** `src/storage/`
**Related phases:** (core infrastructure)
**Last reviewed:** 2026-04-16
