# fs_sim

File system simulator. Format a virtual disk, create files and folders, and see
which blocks they take. Supports FAT12, FAT16 and FAT32.

Try it in the browser: https://xcemaxx.github.io/fs_sim/

## Run

```sh
cargo run                                  # native window
rustup target add wasm32-unknown-unknown
cargo install trunk
trunk serve                                # browser, http://127.0.0.1:8080/
```

Earlier versions: [history](history/).

License: 0BSD. The bundled JetBrains Mono font is under the
[SIL Open Font License 1.1](fs_gui/assets/OFL.txt).
