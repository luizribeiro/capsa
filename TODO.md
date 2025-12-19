# TODO

Remaining architectural improvements from the architecture review.

## HIGH: Simplify IPC Type Duplication

**Issue:** `capsa-apple-vzd-ipc` defines its own types that duplicate `capsa-core`:

| IPC Type | Core Type |
|----------|-----------|
| `capsa_apple_vzd_ipc::VmConfig` | `capsa_core::InternalVmConfig` |
| `capsa_apple_vzd_ipc::DiskConfig` | `capsa_core::DiskImage` |
| `capsa_apple_vzd_ipc::SharedDirConfig` | `capsa_core::SharedDir` |
| `capsa_apple_vzd_ipc::NetworkMode` | `capsa_core::NetworkMode` |

This requires manual conversion in `capsa-apple-vzd/src/main.rs:106-141`.

**Solution:**

1. Add `capsa-core` as a dependency of `capsa-apple-vzd-ipc`
2. Re-export core types from the IPC crate (or use them directly)
3. Delete the duplicate struct definitions from `capsa-apple-vzd-ipc/src/lib.rs`
4. Remove the `convert_config()` function from `capsa-apple-vzd/src/main.rs`

**Impact:** Eliminates ~35 lines of struct definitions + ~35 lines of conversion logic.
