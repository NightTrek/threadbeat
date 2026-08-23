# Runtime notices

Threadbeat release binaries are built with Rust 1.98.0 and statically include portions of the Rust standard library. Rust is distributed under the Apache License 2.0 or MIT License at the recipient's option, while bundled standard-library components also carry the licenses and notices collected in Rust's generated `COPYRIGHT-library.html`. The exact Rust 1.98.0 files are preserved here and shipped identically in every target archive:

| File | SHA-256 |
|---|---|
| `Rust-1.98.0-LICENSE-APACHE` | `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a` |
| `Rust-1.98.0-LICENSE-MIT` | `b71bd43a069ca0641a9ecfe585ca7b3c53b5cc1608f8b68321168698e28b5ea1` |
| `Rust-1.98.0-COPYRIGHT-library.html` | `68129500b616d5838629e68f55ff3aed5e096dacf60ce9eb41bbe599a563afa6` |

The binaries also include SQLite through `rusqlite`'s bundled build. SQLite is a work dedicated to the public domain by its authors. Cargo dependency notices and exact crate license files are generated separately in each bundle's `THIRD_PARTY_LICENSES.md`, `THIRD_PARTY_LICENSES.json`, and `third-party-licenses/` directory.
