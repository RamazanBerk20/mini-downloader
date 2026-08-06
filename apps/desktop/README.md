# Legacy desktop reference

This is the former Tauri/Svelte shell. It is not a Cargo workspace member, is
not built by CI, and is not shipped. The native Slint desktop is in
[`../native-desktop`](../native-desktop).

The native locale loader temporarily reuses the ten translation catalogs in
`src/locales`; keep those catalogs until they are moved to a framework-neutral
resource directory.
