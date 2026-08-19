# CMake TUI Tool

A Cursive-based terminal UI for configuring and building CMake projects. It
auto-detects toolchains (GCC/MinGW, LLVM Clang, clang-cl, MSVC), lets you pick
toolchain/build type/target, and runs CMake configure/build while streaming
output live.

## Features

- **Toolchain auto-detection**: Finds GCC (MinGW), LLVM Clang (GNU CLI and MSVC
  CLI), and MSVC via `vswhere` and common install paths.
- **Generator/toolset matching**: Uses Visual Studio generator for MSVC/clang-cl
  and MinGW Makefiles for GCC/Clang to avoid mismatched compiler/generator.
- **Configure / Build / Stop**: Long-running configure/build operations can be
  interrupted from the same action button (`Configure -> Stop Configure`,
  `Build -> Stop Build`).
- **Delete and Configure**: Removes `build` directory first, then configures.
- **Clean and Build**: Runs clean target first, then build.
- **Auto-configure on build**: If `build` folder does not exist, `Build` will
  auto-run configure before building.
- **Target handling**: Reads targets from generator output, keeps `all` and
  `install` prioritized, and preserves selected target when possible.
- **Workspace-aware persistence**: Stores per-workspace build type and target
  history with LRU-like pruning.
- **Live output + copy**: Streams command output in real time and supports copy.
- **Settings**: Configure parallel jobs and trigger toolchain rescan.

## Usage

Run the TUI from the root of your CMake project:

```sh
cmake-tui-tool
```

Pick a toolchain, build type and target, then press **Configure** and **Build**.

### Interaction

- Use Cursive's standard keyboard navigation to move focus and select options.
- Trigger actions through the bottom action buttons (`Configure`, `Build`,
  `Delete and Configure`, `Clean and Build`, `Copy Output`, `Settings`,
  `Exit`).

### Config files

Config files are stored next to the executable under `config/`:

- `config/toolchain.json`: detected toolchain cache
- `config/config.json`: app settings and workspace build history

`config/config.json` currently includes:

- `parallel_jobs`
- `workspace_history_limit`
- `workspace_builds` (per-workspace `build_type`/`target` and timestamp)

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
