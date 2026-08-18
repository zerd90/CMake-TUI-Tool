# CMake TUI Tool

A terminal UI for configuring and building CMake projects. It auto-detects the
toolchains installed on your system (GCC/MinGW, LLVM Clang, clang-cl, MSVC), lets
you pick a toolchain / build type / target, and runs CMake configure & build while
streaming the output into a scrollable window.

## Features

- **Toolchain auto-detection** — finds GCC (MinGW), LLVM Clang (GNU CLI &
  MSVC CLI), and MSVC (all host/target combinations) via `vswhere` and common
  install paths.
- **Correct generator selection** — uses the Visual Studio generator for
  MSVC/clang-cl (`-G "Visual Studio 18 2026" -A <arch> -T <toolset>`) and
  `MinGW Makefiles` for GCC/Clang, so the selected compiler is actually used.
- **Configure / Build** — runs `cmake -S ... -B ...` with
  `-DCMAKE_EXPORT_COMPILE_COMMANDS=TRUE` and the chosen build type, then
  `cmake --build` (with `--config` for multi-config generators).
- **Target selection** — lists build targets, including `all` and `install`.
- **State restore** — reads an existing `build/CMakeCache.txt` to restore the
  previously used toolchain and build type.
- **Live output** — streams CMake output into a scrollable window; copy it to the
  clipboard with one key press.
- **Settings** — rescan toolchains; selections are cached in `config/toolchain.json`
  next to the executable.
- **CLI scanner** — inspect detected toolchains or benchmark them against CMake
  Tools kits.

## Usage

Run the TUI from the root of your CMake project:

```sh
cmake-tui-tool
```

Pick a toolchain, build type and target, then press **Configure** and **Build**.

### CLI

```sh
cargo run --bin scan -- scan                     # list detected toolchains
cargo run --bin scan -- scan --json              # machine-readable JSON
cargo run --bin scan -- benchmark [--kits PATH]  # compare vs cmake-tools-kits.json
```

## Build

Requires Rust (2024 edition) and Cargo.

```sh
cargo build --release
```

Or use the packaging scripts to produce distributable archives:

```sh
script/build.ps1   # Windows   -> target/dist/*.zip
script/build.sh    # Linux/macOS -> target/dist/*.tar.gz
```

## License

[MIT](LICENSE)
