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
- Open the command panel (`p`) to run `Configure`, `Build`,
  `Delete and Configure`, or `Clean and Build`.
- Use the status bar buttons for copy, settings, command panel, and exit.

### Keyboard shortcuts

- `p`: open command panel
- `Esc`: close command panel or settings window
- `g`: focus and open toolchain dropdown
- `b`: focus and open build type dropdown
- `t`: focus and open target dropdown
- `c`: copy current build output
- `s`: open settings window
- `q`: quit application
- `i`: stop configure/build

### Config files

Config files are stored under the platform config directory in `cmake-tui-tool/`:

- Linux example: `~/.config/cmake-tui-tool/`
- macOS example: `~/Library/Application Support/cmake-tui-tool/`
- Windows example: `%APPDATA%\\cmake-tui-tool\\`

- `toolchain.json`: detected toolchain cache
- `config.json`: app settings and workspace build history

`config.json` currently includes:

- `parallel_jobs`
- `workspace_history_limit`
- `workspace_builds` (per-workspace `build_type`/`target` and timestamp)

### CLI

```sh
cargo run --bin scan -- scan                     # list detected toolchains
cargo run --bin scan -- scan --json              # machine-readable JSON
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
