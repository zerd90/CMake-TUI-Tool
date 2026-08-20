use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(unix)]
use std::time::Duration;

use cursive::event::{Event, Key};
use cursive::theme::{BaseColor, BorderStyle, Color, PaletteColor, Theme};
use cursive::traits::{Nameable, Resizable};
use cursive::views::{
    Button, Dialog, DummyView, EditView, Layer, LinearLayout, NamedView, OnEventView, Panel,
    ScrollView, SelectView, TextView, ThemedView,
};
use cursive::{CbSink, Cursive};

use cmake_tui_tool::runtime::{RunResult, run_command_with_cancel};
use cmake_tui_tool::{
    AppConfig, Toolchain, compiler_stem, configure_compiler_args, is_multi_config, load_app_config,
    load_config, parse_cmake_cache, save_app_config, save_config, scan_toolchains,
};

#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
#[cfg(windows)]
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
};

#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsRawFd;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Configure,
    Build,
}

struct RunningState {
    kind: RunKind,
    stop: Arc<AtomicBool>,
}

struct UiState {
    app_config: AppConfig,
    running: Option<RunningState>,
    exit_notice: Option<String>,
    terminal_bg: Option<Color>,
}

type OutputScrollNamedView = NamedView<ScrollView<NamedView<TextView>>>;

const MIN_TERMINAL_HEIGHT: usize = 16;

fn terminal_height() -> Option<usize> {
    crossterm::terminal::size().ok().map(|(_, h)| h as usize)
}

fn reject_startup_if_too_small() {
    if let Some(height) = terminal_height()
        && height < MIN_TERMINAL_HEIGHT
    {
        eprintln!(
            "Terminal height is too small: {height}. Need at least {MIN_TERMINAL_HEIGHT} rows."
        );
        exit(1);
    }
}

fn enforce_terminal_height_policy(siv: &mut Cursive, height: usize) {
    if height < MIN_TERMINAL_HEIGHT {
        let msg =
            format!("Terminal height {height} is below minimum {MIN_TERMINAL_HEIGHT}; exiting");
        if let Some(state) = siv.user_data::<UiState>() {
            state.exit_notice = Some(msg.clone());
        }
        set_status(siv, msg);
        siv.quit();
    }
}

fn workspace_leaf_name() -> String {
    let dir = std::env::current_dir().unwrap_or_default();
    dir.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "workspace".to_string())
}

fn resolve_project_name() -> String {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let cache = parse_cmake_cache(&project_dir.join("build").join("CMakeCache.txt"));
    cache
        .get("CMAKE_PROJECT_NAME")
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .unwrap_or_else(workspace_leaf_name)
}

fn refresh_project_name_line(siv: &mut Cursive) {
    let name = resolve_project_name();
    siv.call_on_name("project_name", |v: &mut TextView| {
        v.set_content(format!("Project: {name}"));
    });
}

#[cfg(windows)]
fn detect_terminal_background_color() -> Option<Color> {
    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };

    let ok = unsafe { GetConsoleScreenBufferInfo(handle, &mut info) };
    if ok == 0 {
        return None;
    }

    let bg = (info.wAttributes >> 4) & 0x0f;
    let intense = (bg & 0b1000) != 0;
    let rgb = bg & 0b0111;

    let base = match rgb {
        0 => BaseColor::Black,
        1 => BaseColor::Blue,
        2 => BaseColor::Green,
        3 => BaseColor::Cyan,
        4 => BaseColor::Red,
        5 => BaseColor::Magenta,
        6 => BaseColor::Yellow,
        _ => BaseColor::White,
    };

    Some(if intense {
        Color::Light(base)
    } else {
        Color::Dark(base)
    })
}

#[cfg(all(unix, not(windows)))]
fn detect_terminal_background_color() -> Option<Color> {
    detect_bg_from_colorfgbg().or_else(detect_bg_from_osc11)
}

#[cfg(all(not(unix), not(windows)))]
fn detect_terminal_background_color() -> Option<Color> {
    None
}

#[cfg(unix)]
fn color_from_ansi_index(index: u8) -> Color {
    let base = match index & 0b111 {
        0 => BaseColor::Black,
        1 => BaseColor::Red,
        2 => BaseColor::Green,
        3 => BaseColor::Yellow,
        4 => BaseColor::Blue,
        5 => BaseColor::Magenta,
        6 => BaseColor::Cyan,
        _ => BaseColor::White,
    };
    if (index & 0b1000) != 0 {
        Color::Light(base)
    } else {
        Color::Dark(base)
    }
}

#[cfg(unix)]
fn detect_bg_from_colorfgbg() -> Option<Color> {
    let value = std::env::var("COLORFGBG").ok()?;
    let bg = value.split(';').next_back()?.trim().parse::<u8>().ok()?;
    (bg <= 15).then(|| color_from_ansi_index(bg))
}

#[cfg(unix)]
fn scale_hex_channel_to_u8(hex: &str) -> Option<u8> {
    if hex.is_empty() || hex.len() > 4 {
        return None;
    }
    let value = u16::from_str_radix(hex, 16).ok()? as u32;
    let max = ((1u32 << (hex.len() * 4)) - 1).max(1);
    Some(((value * 255) / max) as u8)
}

#[cfg(unix)]
fn parse_osc11_rgb(reply: &str) -> Option<Color> {
    let start = reply.find("rgb:")? + 4;
    let mut tail = &reply[start..];
    if let Some(i) = tail.find('\u{7}') {
        tail = &tail[..i];
    }
    if let Some(i) = tail.find("\x1b\\") {
        tail = &tail[..i];
    }
    let mut parts = tail.split('/');
    let r = scale_hex_channel_to_u8(parts.next()?.trim())?;
    let g = scale_hex_channel_to_u8(parts.next()?.trim())?;
    let b = scale_hex_channel_to_u8(parts.next()?.trim())?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(unix)]
struct TermiosGuard {
    fd: i32,
    original: libc::termios,
    active: bool,
}

#[cfg(unix)]
impl Drop for TermiosGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                let _ = libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
            }
        }
    }
}

#[cfg(unix)]
fn detect_bg_from_osc11() -> Option<Color> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .ok()?;
    let fd = tty.as_raw_fd();

    let mut original: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return None;
    }
    let mut raw = original;
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    let _guard = TermiosGuard {
        fd,
        original,
        active: true,
    };

    if tty.write_all(b"\x1b]11;?\x07").is_err() || tty.flush().is_err() {
        return None;
    }

    let deadline = Instant::now() + Duration::from_millis(180);
    let mut data = Vec::new();
    while Instant::now() < deadline {
        let remain = (deadline - Instant::now()).as_millis().min(60) as i32;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut pfd, 1, remain) };
        if polled <= 0 {
            continue;
        }

        let mut buf = [0u8; 256];
        let n = tty.read(&mut buf).ok()?;
        if n == 0 {
            continue;
        }
        data.extend_from_slice(&buf[..n]);

        if data.windows(2).any(|w| w == b"\x1b\\") || data.contains(&0x07) {
            break;
        }
    }

    if data.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&data);
    parse_osc11_rgb(&text)
}

fn base_color_rgb(base: BaseColor, light: bool) -> (u8, u8, u8) {
    match (base, light) {
        (BaseColor::Black, false) => (0, 0, 0),
        (BaseColor::Black, true) => (96, 96, 96),
        (BaseColor::Red, false) => (170, 0, 0),
        (BaseColor::Red, true) => (255, 96, 96),
        (BaseColor::Green, false) => (0, 170, 0),
        (BaseColor::Green, true) => (96, 255, 96),
        (BaseColor::Yellow, false) => (170, 85, 0),
        (BaseColor::Yellow, true) => (255, 255, 96),
        (BaseColor::Blue, false) => (0, 0, 170),
        (BaseColor::Blue, true) => (96, 160, 255),
        (BaseColor::Magenta, false) => (170, 0, 170),
        (BaseColor::Magenta, true) => (255, 96, 255),
        (BaseColor::Cyan, false) => (0, 170, 170),
        (BaseColor::Cyan, true) => (96, 255, 255),
        (BaseColor::White, false) => (170, 170, 170),
        (BaseColor::White, true) => (255, 255, 255),
    }
}

fn color_luma(color: Color) -> Option<u16> {
    let (r, g, b) = match color {
        Color::Dark(base) => base_color_rgb(base, false),
        Color::Light(base) => base_color_rgb(base, true),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::RgbLowRes(r, g, b) => {
            let to_255 = |v: u8| -> u8 {
                let clamped = v.min(5) as u16;
                ((clamped * 255) / 5) as u8
            };
            (to_255(r), to_255(g), to_255(b))
        }
        Color::TerminalDefault => return None,
    };

    // ITU-R BT.709 luma approximation scaled to 0..255.
    let luma = (2126u32 * r as u32 + 7152u32 * g as u32 + 722u32 * b as u32) / 10_000;
    Some(luma as u16)
}

fn is_light_background(bg: Color) -> bool {
    color_luma(bg).map(|l| l >= 145).unwrap_or(false)
}

const COMMAND_PANEL_LIGHT_BG: Color = Color::Rgb(236, 236, 236);
const COMMAND_PANEL_DARK_BG: Color = Color::Rgb(24, 24, 24);

fn command_panel_bg_color(terminal_bg: Option<Color>, fallback_bg: Color) -> Color {
    let bg = terminal_bg.unwrap_or(fallback_bg);
    if is_light_background(bg) {
        COMMAND_PANEL_LIGHT_BG
    } else {
        COMMAND_PANEL_DARK_BG
    }
}

fn command_panel_theme(base_theme: &Theme, terminal_bg: Option<Color>) -> Theme {
    let mut theme = base_theme.clone();
    let panel_bg = command_panel_bg_color(terminal_bg, theme.palette[PaletteColor::Background]);
    theme.palette[PaletteColor::Background] = panel_bg;
    theme.palette[PaletteColor::View] = panel_bg;
    theme.palette[PaletteColor::HighlightInactive] = panel_bg;
    theme
}

fn apply_ui_theme(siv: &mut Cursive, terminal_bg: Option<Color>) {
    let mut theme = Theme::default();
    theme.shadow = false;
    theme.borders = BorderStyle::Simple;

    let bg = terminal_bg.unwrap_or(Color::Dark(BaseColor::Black));
    let light_bg = is_light_background(bg);

    theme.palette[PaletteColor::Background] = bg;
    theme.palette[PaletteColor::View] = bg;
    if light_bg {
        theme.palette[PaletteColor::Primary] = Color::Dark(BaseColor::Black);
        theme.palette[PaletteColor::Secondary] = Color::Dark(BaseColor::Blue);
        theme.palette[PaletteColor::TitlePrimary] = Color::Dark(BaseColor::Blue);
        theme.palette[PaletteColor::TitleSecondary] = Color::Dark(BaseColor::Green);
        theme.palette[PaletteColor::Highlight] = Color::Dark(BaseColor::Blue);
        theme.palette[PaletteColor::HighlightText] = Color::Light(BaseColor::White);
    } else {
        theme.palette[PaletteColor::Primary] = Color::Light(BaseColor::White);
        theme.palette[PaletteColor::Secondary] = Color::Light(BaseColor::Cyan);
        theme.palette[PaletteColor::TitlePrimary] = Color::Light(BaseColor::Cyan);
        theme.palette[PaletteColor::TitleSecondary] = Color::Light(BaseColor::Green);
        theme.palette[PaletteColor::Highlight] = Color::Dark(BaseColor::Cyan);
        theme.palette[PaletteColor::HighlightText] = Color::Dark(BaseColor::Black);
    }
    theme.palette[PaletteColor::HighlightInactive] = bg;

    siv.set_theme(theme);
}

fn populate_toolchains(siv: &mut Cursive, list: Vec<Toolchain>) {
    siv.call_on_name("toolchain", |v: &mut SelectView<Toolchain>| {
        v.clear();
        for tc in list {
            let mut label = format!("{} ({})", tc.name, tc.version);
            if let Some(tri) = &tc.triple {
                label.push_str(&format!(" [{}]", tri));
            } else if let Some(a) = &tc.arch {
                label.push_str(&format!(" [{}]", a));
            }
            label.push_str(&format!("  {}", tc.path));
            v.add_item(label, tc);
        }
    });
}

fn select_by_value(siv: &mut Cursive, name: &str, target: &str) {
    siv.call_on_name(name, |v: &mut SelectView<String>| {
        for i in 0..v.len() {
            if let Some((_, value)) = v.get_item(i)
                && value.as_str() == target
            {
                v.set_selection(i);
                break;
            }
        }
    });
}

fn select_toolchain_by_stem(siv: &mut Cursive, stem: &str) {
    siv.call_on_name("toolchain", |v: &mut SelectView<Toolchain>| {
        for i in 0..v.len() {
            if let Some((_, value)) = v.get_item(i)
                && compiler_stem(&value.path).as_deref() == Some(stem)
            {
                v.set_selection(i);
                break;
            }
        }
    });
}

fn select_toolchain_by_id_arch(siv: &mut Cursive, id: &str, arch: &str) {
    siv.call_on_name("toolchain", |v: &mut SelectView<Toolchain>| {
        for i in 0..v.len() {
            if let Some((_, value)) = v.get_item(i)
                && value.id == id
                && (arch.is_empty() || value.arch.as_deref() == Some(arch))
            {
                v.set_selection(i);
                break;
            }
        }
    });
}

fn restore_from_cache(siv: &mut Cursive) {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let cache = parse_cmake_cache(&project_dir.join("build").join("CMakeCache.txt"));
    if cache.is_empty() {
        return;
    }

    if let Some(bt) = cache.get("CMAKE_BUILD_TYPE")
        && !bt.is_empty()
    {
        select_by_value(siv, "build_type", &bt.to_lowercase());
    }

    let generator = cache.get("CMAKE_GENERATOR").cloned().unwrap_or_default();

    if generator.contains("Visual Studio") {
        // VS generator: the compiler is implicit; identify it via toolset + platform.
        let toolset = cache
            .get("CMAKE_GENERATOR_TOOLSET")
            .map(|s| s.as_str())
            .unwrap_or("");
        let arch = cache
            .get("CMAKE_GENERATOR_PLATFORM")
            .map(|s| s.as_str())
            .unwrap_or("");
        let id = if toolset.contains("ClangCL") {
            "clang-cl"
        } else {
            "msvc"
        };
        select_toolchain_by_id_arch(siv, id, arch);
    } else {
        // Makefile / Ninja generator: match by compiler executable stem.
        let compiler = cache
            .get("CMAKE_C_COMPILER")
            .filter(|v| !v.is_empty() && !v.contains("NOTFOUND"))
            .or_else(|| cache.get("CMAKE_CXX_COMPILER"));
        if let Some(compiler) = compiler
            && let Some(stem) = compiler_stem(compiler)
        {
            select_toolchain_by_stem(siv, &stem);
        }
    }
}

fn parse_cmake_target_help(output: &str) -> Vec<String> {
    const BUILTIN: &[&str] = &[
        "clean",
        "help",
        "depend",
        "edit_cache",
        "rebuild_cache",
        "list_install_components",
        "package",
        "package_source",
    ];

    let mut targets = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        // Ninja: "target_name: phony"
        // Makefiles (Unix/MinGW/NMake): "... target_name"
        let name = if let Some(name) = line.strip_suffix(": phony") {
            name.trim()
        } else if let Some(name) = line.strip_prefix("... ") {
            name.trim()
        } else {
            continue;
        };
        let name = name.trim_end_matches(" (the default if no target is provided)");
        if BUILTIN.contains(&name) {
            continue;
        }
        if name.contains('/') || name.contains('\\') {
            continue;
        }
        if name.contains('.') {
            continue;
        }
        if !targets.contains(&name.to_string()) {
            targets.push(name.to_string());
        }
    }
    targets
}

fn scan_targets_from_cmake_help(build_dir: &Path) -> Vec<String> {
    let out = match std::process::Command::new("cmake")
        .arg("--build")
        .arg(build_dir)
        .arg("--target")
        .arg("help")
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    parse_cmake_target_help(&format!("{}\n{}", stdout, stderr))
}

fn extract_xml_attr(s: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=\"", name);
    let start = s.find(&pattern)? + pattern.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn scan_targets_from_slnx(build_dir: &Path) -> Vec<String> {
    const BUILTIN: &[&str] = &["ZERO_CHECK"];

    let entries = match std::fs::read_dir(build_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let slnx = entries
        .flatten()
        .find(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("slnx"))
        })
        .map(|e| e.path());
    let slnx = match slnx {
        Some(p) => p,
        None => return Vec::new(),
    };
    let content = match std::fs::read_to_string(slnx) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut targets = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if !line.starts_with("<Project") {
            continue;
        }
        let Some(path) = extract_xml_attr(line, "Path") else {
            continue;
        };
        let Some(file_name) = Path::new(&path).file_name() else {
            continue;
        };
        let name = file_name.to_string_lossy();
        let name = name.trim_end_matches(".vcxproj");
        if name.is_empty() || BUILTIN.contains(&name) {
            continue;
        }
        let name = match name {
            "ALL_BUILD" => "all",
            "INSTALL" => "install",
            other => other,
        };
        if !targets.contains(&name.to_string()) {
            targets.push(name.to_string());
        }
    }
    targets
}

fn scan_targets_from_build(build_dir: &Path) -> Vec<String> {
    let cache = parse_cmake_cache(&build_dir.join("CMakeCache.txt"));
    let generator = cache
        .get("CMAKE_GENERATOR")
        .map(|s| s.as_str())
        .unwrap_or("");

    if generator.contains("Visual Studio") {
        return scan_targets_from_slnx(build_dir);
    }

    scan_targets_from_cmake_help(build_dir)
}

fn populate_targets(siv: &mut Cursive, targets: Vec<String>) {
    let targets = prioritize_targets(targets);
    siv.call_on_name("target", |v: &mut SelectView<String>| {
        v.clear();
        for t in targets {
            v.add_item(t.clone(), t);
        }
    });
}

fn prioritize_targets(targets: Vec<String>) -> Vec<String> {
    let mut all = None;
    let mut install = None;
    let mut rest = Vec::new();
    for t in targets {
        if t == "all" && all.is_none() {
            all = Some(t);
        } else if t == "install" && install.is_none() {
            install = Some(t);
        } else {
            rest.push(t);
        }
    }

    let mut ordered = Vec::new();
    if let Some(t) = all {
        ordered.push(t);
    }
    if let Some(t) = install {
        ordered.push(t);
    }
    ordered.extend(rest);
    ordered
}

fn current_target_selection_state(siv: &mut Cursive) -> (bool, String) {
    let mut has_items = false;
    let mut selected = String::new();
    siv.call_on_name("target", |v: &mut SelectView<String>| {
        has_items = v.len() > 0;
        if let Some(sel) = v.selection() {
            selected = sel.to_string();
        }
    });
    (has_items, selected)
}

fn current_workspace_dir() -> std::path::PathBuf {
    std::env::current_dir().unwrap_or_default()
}

fn restore_build_type_from_workspace_history(siv: &mut Cursive) {
    let workspace = current_workspace_dir();
    let saved_build_type = siv
        .user_data::<UiState>()
        .and_then(|state| state.app_config.workspace_build(&workspace))
        .map(|entry| entry.build_type.clone())
        .unwrap_or_default();
    if !saved_build_type.is_empty() {
        select_by_value(siv, "build_type", &saved_build_type);
    }
}

fn restore_target_from_workspace_history(siv: &mut Cursive) {
    let workspace = current_workspace_dir();
    let saved_target = siv
        .user_data::<UiState>()
        .and_then(|state| state.app_config.workspace_build(&workspace))
        .map(|entry| entry.target.clone())
        .unwrap_or_default();
    if !saved_target.is_empty() {
        select_by_value(siv, "target", &saved_target);
    }
}

fn remember_workspace_build_selection(siv: &mut Cursive, build_type: &str, target: &str) {
    let workspace = current_workspace_dir();
    if let Some(state) = siv.user_data::<UiState>() {
        state
            .app_config
            .remember_workspace_build(&workspace, build_type, target);
        save_app_config(&state.app_config);
    }
}

fn populate_targets_preserve(siv: &mut Cursive, targets: Vec<String>, selected: Option<String>) {
    let keep = selected.unwrap_or_default();
    populate_targets(siv, targets);
    if !keep.is_empty() {
        select_by_value(siv, "target", &keep);
    }
}

fn selection(siv: &mut Cursive, name: &str) -> String {
    let mut value = String::new();
    siv.call_on_name(name, |v: &mut SelectView<String>| {
        if let Some(sel) = v.selection() {
            value = sel.to_string();
        }
    });
    value
}

fn selected_toolchain(siv: &mut Cursive) -> Option<Toolchain> {
    let mut out = None;
    siv.call_on_name("toolchain", |v: &mut SelectView<Toolchain>| {
        out = v.selection().map(|t| t.as_ref().clone());
    });
    out
}

fn set_status(siv: &mut Cursive, msg: impl Into<String>) {
    siv.call_on_name("status", |v: &mut TextView| v.set_content(msg));
}

fn copy_to_clipboard(text: &str) -> bool {
    use base64::Engine;

    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{}\x1b\\", encoded);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(osc.as_bytes()).is_ok() && stdout.flush().is_ok()
}

fn on_copy_output(siv: &mut Cursive) {
    let mut text = String::new();
    siv.call_on_name("output", |v: &mut TextView| {
        text = v.get_content().source().to_string();
    });
    if text.is_empty() {
        set_status(siv, "Nothing to copy");
        return;
    }
    if copy_to_clipboard(&text) {
        set_status(siv, format!("Copied {} bytes to clipboard", text.len()));
    } else {
        set_status(siv, "Failed to copy to clipboard");
    }
}

fn clear_output(siv: &mut Cursive) {
    siv.call_on_name("output", |v: &mut TextView| v.set_content(""));
}

fn append_output(siv: &mut Cursive, line: impl Into<String>) {
    siv.call_on_name("output", |v: &mut TextView| {
        let mut content = v.get_content().source().to_string();
        content.push_str(&line.into());
        content.push('\n');
        v.set_content(content);
    });
    siv.call_on_name("output_scroll", |v: &mut OutputScrollNamedView| {
        v.get_mut().scroll_to_bottom();
    });
}

fn reset_action_buttons(siv: &mut Cursive) {
    siv.call_on_name("btn_configure", |v: &mut Button| {
        v.set_label("Configure");
        v.set_enabled(true);
    });
    siv.call_on_name("btn_build", |v: &mut Button| {
        v.set_label("Build");
        v.set_enabled(true);
    });
}

fn set_running_buttons(siv: &mut Cursive, kind: RunKind) {
    match kind {
        RunKind::Configure => {
            siv.call_on_name("btn_configure", |v: &mut Button| {
                v.set_label("Stop Configure");
                v.set_enabled(true);
            });
            siv.call_on_name("btn_build", |v: &mut Button| v.set_enabled(false));
            siv.call_on_name("btn_delete_configure", |v: &mut Button| {
                v.set_enabled(false)
            });
            siv.call_on_name("btn_clean_build", |v: &mut Button| v.set_enabled(false));
        }
        RunKind::Build => {
            siv.call_on_name("btn_build", |v: &mut Button| {
                v.set_label("Stop Build");
                v.set_enabled(true);
            });
            siv.call_on_name("btn_configure", |v: &mut Button| v.set_enabled(false));
            siv.call_on_name("btn_delete_configure", |v: &mut Button| {
                v.set_enabled(false)
            });
            siv.call_on_name("btn_clean_build", |v: &mut Button| v.set_enabled(false));
        }
    }
}

fn start_running(siv: &mut Cursive, kind: RunKind) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    if let Some(state) = siv.user_data::<UiState>() {
        state.running = Some(RunningState {
            kind,
            stop: Arc::clone(&stop),
        });
    }
    set_running_buttons(siv, kind);
    stop
}

fn finish_running(siv: &mut Cursive) {
    if let Some(state) = siv.user_data::<UiState>() {
        state.running = None;
    }
    reset_action_buttons(siv);
    siv.call_on_name("btn_delete_configure", |v: &mut Button| v.set_enabled(true));
    siv.call_on_name("btn_clean_build", |v: &mut Button| v.set_enabled(true));
}

fn request_stop(siv: &mut Cursive, kind: RunKind) -> bool {
    if let Some(state) = siv.user_data::<UiState>()
        && let Some(running) = &state.running
        && running.kind == kind
    {
        running.stop.store(true, Ordering::SeqCst);
        return true;
    }
    false
}

fn running_kind(siv: &mut Cursive) -> Option<RunKind> {
    siv.user_data::<UiState>()
        .and_then(|state| state.running.as_ref().map(|r| r.kind))
}

fn run_cmake_command(
    sink: &CbSink,
    cmd: std::process::Command,
    header: &str,
    stop: Arc<AtomicBool>,
) -> RunResult {
    if !header.is_empty() {
        let sink = sink.clone();
        let header = header.to_string();
        let _ = sink.send(Box::new(move |siv| {
            append_output(siv, header);
        }));
    }
    let sink_for_line = sink.clone();
    let on_line = Arc::new(move |line: String| {
        let sink = sink_for_line.clone();
        let _ = sink.send(Box::new(move |siv| {
            append_output(siv, line);
        }));
    });

    match run_command_with_cancel(cmd, stop, on_line) {
        Ok(result) => result,
        Err(e) => {
            let msg = format!("Failed to start cmake: {e}");
            let sink = sink.clone();
            let _ = sink.send(Box::new(move |siv| set_status(siv, msg)));
            RunResult::Failed
        }
    }
}

fn clear_build_cache(build_dir: &Path) {
    let _ = std::fs::remove_file(build_dir.join("CMakeCache.txt"));
    let _ = std::fs::remove_dir_all(build_dir.join("CMakeFiles"));
}

fn which(name: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let bare = dir.join(name);
        if bare.is_file() {
            return Some(bare.to_string_lossy().to_string());
        }
        let with_exe = dir.join(format!("{name}.exe"));
        if with_exe.is_file() {
            return Some(with_exe.to_string_lossy().to_string());
        }
    }
    None
}

const VS_ENV_VARS: &[&str] = &[
    "CMAKE_GENERATOR",
    "CMAKE_GENERATOR_INSTANCE",
    "CMAKE_GENERATOR_PLATFORM",
    "CMAKE_GENERATOR_TOOLSET",
    "VCToolsInstallDir",
    "VCToolsVersion",
    "VCINSTALLDIR",
    "DevEnvDir",
    "FrameworkDir",
    "FrameworkVersion",
    "FrameworkVer",
    "WindowsSdkDir",
    "WindowsSdkVerBinPath",
    "WindowsSdkBinPath",
    "WindowsLibPath",
    "UCRTVersion",
    "UniversalCRTSdkDir",
    "ExtensionSdkDir",
    "INCLUDE",
    "LIB",
    "LIBPATH",
    "CL",
    "_LINK_",
];

fn clean_cmake_env(cmd: &mut std::process::Command) {
    // Rebuild the child environment from the current process env with
    // case-insensitive de-duplication. Environment blocks produced by
    // MSYS2/Git-Bash-style launchers can carry both "LANG" and "Lang",
    // which makes MSBuild's case-insensitive environment dictionary throw
    // "Item has already been added".
    let mut seen = HashSet::new();
    let mut filtered: Vec<(String, String)> = Vec::new();
    for (key, val) in std::env::vars_os() {
        let key = key.to_string_lossy().to_string();
        let lower = key.to_lowercase();
        if !seen.insert(lower) {
            continue;
        }
        if VS_ENV_VARS.contains(&key.as_str()) {
            continue;
        }
        filtered.push((key, val.to_string_lossy().to_string()));
    }
    cmd.env_clear();
    for (key, val) in filtered {
        cmd.env(key, val);
    }
}

fn env_diagnostics() -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "cmake: {}",
        which("cmake").unwrap_or_else(|| "NOT FOUND".to_string())
    ));
    for var in VS_ENV_VARS {
        if let Some(v) = std::env::var_os(var) {
            let s = v.to_string_lossy().to_string();
            let s = if s.len() > 120 {
                format!("{}...", &s[..120])
            } else {
                s
            };
            lines.push(format!("env {var}={s}"));
        }
    }
    lines.join("\n")
}

fn broken_cache(build_dir: &Path) -> bool {
    let cache = parse_cmake_cache(&build_dir.join("CMakeCache.txt"));
    if cache.is_empty() {
        return false;
    }
    let notfound = |k: &str| cache.get(k).is_some_and(|v| v.contains("NOTFOUND"));
    notfound("CMAKE_C_COMPILER")
        || notfound("CMAKE_CXX_COMPILER")
        || notfound("CMAKE_LINKER")
        || notfound("CMAKE_AR")
}

fn stale_cache_conflict(build_dir: &Path, toolchain: &Toolchain) -> Option<String> {
    let cache = parse_cmake_cache(&build_dir.join("CMakeCache.txt"));
    if cache.is_empty() {
        return None;
    }
    let Some(generator) = &toolchain.generator else {
        return None;
    };
    if cache.get("CMAKE_GENERATOR").is_some_and(|g| g != generator) {
        return Some(format!(
            "generator: {} -> {}",
            cache.get("CMAKE_GENERATOR").unwrap(),
            generator
        ));
    }
    if let Some(arch) = &toolchain.arch
        && cache
            .get("CMAKE_GENERATOR_PLATFORM")
            .is_some_and(|p| p != arch)
    {
        return Some(format!(
            "platform: {} -> {}",
            cache.get("CMAKE_GENERATOR_PLATFORM").unwrap(),
            arch
        ));
    }
    if let Some(toolset) = &toolchain.toolset
        && cache
            .get("CMAKE_GENERATOR_TOOLSET")
            .is_some_and(|t| t != toolset)
    {
        return Some(format!(
            "toolset: {} -> {}",
            cache.get("CMAKE_GENERATOR_TOOLSET").unwrap(),
            toolset
        ));
    }
    None
}

fn format_command(args: &[String]) -> String {
    let mut s = String::from("cmake");
    for a in args {
        s.push(' ');
        if a.chars().any(|c| c.is_whitespace()) {
            s.push('"');
            s.push_str(a);
            s.push('"');
        } else {
            s.push_str(a);
        }
    }
    s
}

fn start_configure_flow(siv: &mut Cursive, delete_build_dir_first: bool) {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let build_type = selection(siv, "build_type");
    let (had_targets_before, selected_target_before) = current_target_selection_state(siv);
    let Some(toolchain) = selected_toolchain(siv) else {
        set_status(siv, "No toolchain selected");
        return;
    };
    let toolchain_label = format!("{} ({})", toolchain.name, toolchain.version);
    let sink = siv.cb_sink().clone();
    let stop = start_running(siv, RunKind::Configure);

    clear_output(siv);
    if delete_build_dir_first {
        set_status(
            siv,
            format!(
                "Delete and configure: toolchain={} build_type={}",
                toolchain_label, build_type
            ),
        );
    } else {
        set_status(
            siv,
            format!(
                "Configuring: toolchain={} build_type={}",
                toolchain_label, build_type
            ),
        );
    }

    std::thread::spawn(move || {
        let started = Instant::now();
        let build_dir = project_dir.join("build");
        let mut pre_notes: Vec<String> = Vec::new();

        if delete_build_dir_first {
            pre_notes.push("Delete and configure: removing build directory".to_string());
            if let Err(e) = std::fs::remove_dir_all(&build_dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                pre_notes.push(format!("Failed to remove build directory: {e}"));
            }
        }

        let mut args: Vec<String> = vec![
            "-S".to_string(),
            project_dir.to_string_lossy().to_string(),
            "-B".to_string(),
            build_dir.to_string_lossy().to_string(),
        ];
        if !is_multi_config(&toolchain) {
            args.push(format!("-DCMAKE_BUILD_TYPE={}", build_type));
        }
        args.push("-DCMAKE_EXPORT_COMPILE_COMMANDS:BOOL=TRUE".to_string());
        args.extend(configure_compiler_args(&toolchain));

        if let Some(conflict) = stale_cache_conflict(&build_dir, &toolchain) {
            let msg = format!("Toolchain changed ({conflict}), clearing build cache");
            pre_notes.push(msg);
            clear_build_cache(&build_dir);
        } else if broken_cache(&build_dir) {
            let msg = "Broken build cache detected (compiler/linker NOTFOUND), clearing it";
            pre_notes.push(msg.to_string());
            clear_build_cache(&build_dir);
        }

        let cmdline = format_command(&args);
        let mut header = format!("{}\n$ {cmdline}", env_diagnostics());
        for note in pre_notes {
            header.push('\n');
            header.push_str(&note);
        }

        let mut cmd = std::process::Command::new("cmake");
        cmd.args(&args);
        clean_cmake_env(&mut cmd);

        let result = run_cmake_command(&sink, cmd, &header, stop);

        match result {
            RunResult::Success => {
                let targets = scan_targets_from_build(&build_dir);
                let n = targets.len();
                let elapsed = started.elapsed();
                let prev = if had_targets_before {
                    Some(selected_target_before)
                } else {
                    None
                };
                let _ = sink.send(Box::new(move |siv| {
                    populate_targets_preserve(siv, targets, prev);
                    refresh_project_name_line(siv);
                    let secs = elapsed.as_secs_f32();
                    append_output(siv, format!("Configure finished in {:.2}s", secs));
                    set_status(
                        siv,
                        format!("Configure succeeded: {} target(s) in {:.2}s", n, secs),
                    );
                    finish_running(siv);
                }));
            }
            RunResult::Cancelled => {
                let elapsed = started.elapsed();
                let _ = sink.send(Box::new(move |siv| {
                    let secs = elapsed.as_secs_f32();
                    append_output(siv, format!("Configure stopped after {:.2}s", secs));
                    set_status(siv, format!("Configure stopped after {:.2}s", secs));
                    finish_running(siv);
                }));
            }
            RunResult::Failed => {
                let elapsed = started.elapsed();
                let _ = sink.send(Box::new(move |siv| {
                    let secs = elapsed.as_secs_f32();
                    append_output(siv, format!("Configure failed after {:.2}s", secs));
                    set_status(siv, format!("Configure failed after {:.2}s", secs));
                    finish_running(siv);
                }));
            }
        }
    });
}

fn start_build_flow(siv: &mut Cursive, clean_first: bool) {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let build_dir = project_dir.join("build");
    let needs_configure = !build_dir.exists();
    let target = selection(siv, "target");
    let build_type = selection(siv, "build_type");
    let toolchain = if needs_configure {
        let Some(tc) = selected_toolchain(siv) else {
            set_status(siv, "No build folder and no toolchain selected");
            return;
        };
        Some(tc)
    } else {
        None
    };
    let parallel_jobs = current_parallel_jobs(siv);
    remember_workspace_build_selection(siv, &build_type, &target);
    let sink = siv.cb_sink().clone();
    let stop = start_running(siv, RunKind::Build);

    let label = if target.is_empty() {
        "all".to_string()
    } else {
        target.clone()
    };
    clear_output(siv);
    if clean_first {
        set_status(siv, format!("Cleaning and building '{}'...", label));
    } else {
        set_status(siv, format!("Building '{}'...", label));
    }

    std::thread::spawn(move || {
        let started = Instant::now();
        if needs_configure {
            let toolchain = toolchain.expect("toolchain is required when auto-configuring");
            let mut args: Vec<String> = vec![
                "-S".to_string(),
                project_dir.to_string_lossy().to_string(),
                "-B".to_string(),
                build_dir.to_string_lossy().to_string(),
            ];
            if !is_multi_config(&toolchain) {
                args.push(format!("-DCMAKE_BUILD_TYPE={}", build_type));
            }
            args.push("-DCMAKE_EXPORT_COMPILE_COMMANDS:BOOL=TRUE".to_string());
            args.extend(configure_compiler_args(&toolchain));

            let cmdline = format_command(&args);
            let header = format!(
                "{}\nNo build folder found, auto-running configure before build\n$ {cmdline}",
                env_diagnostics()
            );

            let mut configure_cmd = std::process::Command::new("cmake");
            configure_cmd.args(&args);
            clean_cmake_env(&mut configure_cmd);

            let configure_result =
                run_cmake_command(&sink, configure_cmd, &header, Arc::clone(&stop));
            match configure_result {
                RunResult::Success => {
                    let targets = scan_targets_from_build(&build_dir);
                    let _ = sink.send(Box::new(move |siv| {
                        if !targets.is_empty() {
                            populate_targets(siv, targets);
                        }
                        set_status(siv, "Auto configure succeeded; starting build...");
                    }));
                }
                RunResult::Cancelled => {
                    let _ = sink.send(Box::new(move |siv| {
                        set_status(siv, "Build stopped during auto configure");
                        finish_running(siv);
                    }));
                    return;
                }
                RunResult::Failed => {
                    let _ = sink.send(Box::new(move |siv| {
                        set_status(siv, "Auto configure failed; build skipped");
                        finish_running(siv);
                    }));
                    return;
                }
            }
        }

        let cache = parse_cmake_cache(&build_dir.join("CMakeCache.txt"));
        let is_vs = cache
            .get("CMAKE_GENERATOR")
            .is_some_and(|g| g.contains("Visual Studio"));

        if clean_first {
            let mut clean_cmd = std::process::Command::new("cmake");
            clean_cmd
                .arg("--build")
                .arg(&build_dir)
                .arg("--target")
                .arg("clean");
            if is_vs {
                clean_cmd.arg("--config").arg(&build_type);
            }
            clean_cmake_env(&mut clean_cmd);
            let mut clean_cmdline = format!(
                "$ cmake --build {} --target clean",
                build_dir.to_string_lossy()
            );
            if is_vs {
                clean_cmdline.push_str(&format!(" --config {}", build_type));
            }
            let clean_result =
                run_cmake_command(&sink, clean_cmd, &clean_cmdline, Arc::clone(&stop));
            match clean_result {
                RunResult::Success => {
                    let _ = sink.send(Box::new(move |siv| {
                        set_status(siv, "Clean succeeded; starting build...")
                    }));
                }
                RunResult::Cancelled => {
                    let _ = sink.send(Box::new(move |siv| {
                        set_status(siv, "Build stopped during clean");
                        finish_running(siv);
                    }));
                    return;
                }
                RunResult::Failed => {
                    let _ = sink.send(Box::new(move |siv| {
                        set_status(siv, "Clean failed; build skipped");
                        finish_running(siv);
                    }));
                    return;
                }
            }
        }

        let mut cmd = std::process::Command::new("cmake");
        cmd.arg("--build").arg(&build_dir);
        if !target.is_empty() {
            let t = if is_vs {
                match target.as_str() {
                    "all" => "ALL_BUILD".to_string(),
                    "install" => "INSTALL".to_string(),
                    other => other.to_string(),
                }
            } else {
                target.clone()
            };
            cmd.arg("--target").arg(&t);
        }
        if is_vs {
            cmd.arg("--config").arg(&build_type);
        }
        cmd.arg("--parallel").arg(parallel_jobs.to_string());
        clean_cmake_env(&mut cmd);
        cmd.env("CMAKE_BUILD_PARALLEL_LEVEL", parallel_jobs.to_string());

        let mut build_cmdline = format!("$ cmake --build {}", build_dir.to_string_lossy());
        if !target.is_empty() {
            let t = if is_vs {
                match target.as_str() {
                    "all" => "ALL_BUILD".to_string(),
                    "install" => "INSTALL".to_string(),
                    other => other.to_string(),
                }
            } else {
                target.clone()
            };
            build_cmdline.push_str(&format!(" --target {}", t));
        }
        if is_vs {
            build_cmdline.push_str(&format!(" --config {}", build_type));
        }
        build_cmdline.push_str(&format!(" --parallel {}", parallel_jobs));
        build_cmdline.push_str(&format!("  # CMAKE_BUILD_PARALLEL_LEVEL={}", parallel_jobs));

        let result = run_cmake_command(&sink, cmd, &build_cmdline, stop);

        let elapsed = started.elapsed();
        let _ = sink.send(Box::new(move |siv| {
            let secs = elapsed.as_secs_f32();
            match result {
                RunResult::Success => {
                    append_output(siv, format!("Build finished in {:.2}s", secs));
                    set_status(siv, format!("Build succeeded: {} in {:.2}s", label, secs));
                }
                RunResult::Cancelled => {
                    append_output(siv, format!("Build stopped after {:.2}s", secs));
                    set_status(siv, format!("Build stopped: {} after {:.2}s", label, secs));
                }
                RunResult::Failed => {
                    append_output(siv, format!("Build failed after {:.2}s", secs));
                    set_status(siv, format!("Build failed: {} after {:.2}s", label, secs));
                }
            }
            finish_running(siv);
        }));
    });
}

fn on_configure(siv: &mut Cursive) {
    if let Some(kind) = running_kind(siv) {
        if kind == RunKind::Configure {
            if request_stop(siv, RunKind::Configure) {
                set_status(siv, "Stopping configure...");
            }
            return;
        }
        set_status(siv, "Build is running; stop it first");
        return;
    }

    start_configure_flow(siv, false);
}

fn on_build(siv: &mut Cursive) {
    if let Some(kind) = running_kind(siv) {
        if kind == RunKind::Build {
            if request_stop(siv, RunKind::Build) {
                set_status(siv, "Stopping build...");
            }
            return;
        }
        set_status(siv, "Configure is running; stop it first");
        return;
    }

    start_build_flow(siv, false);
}

fn on_delete_and_configure(siv: &mut Cursive) {
    if let Some(kind) = running_kind(siv) {
        if kind == RunKind::Configure {
            set_status(siv, "Configure is running; use Stop Configure");
            return;
        }
        set_status(siv, "Build is running; stop it first");
        return;
    }

    start_configure_flow(siv, true);
}

fn on_clean_and_build(siv: &mut Cursive) {
    if let Some(kind) = running_kind(siv) {
        if kind == RunKind::Build {
            set_status(siv, "Build is running; use Stop Build");
            return;
        }
        set_status(siv, "Configure is running; stop it first");
        return;
    }

    start_build_flow(siv, true);
}

fn current_parallel_jobs(siv: &mut Cursive) -> usize {
    siv.user_data::<UiState>()
        .map(|s| s.app_config.parallel_jobs.max(1))
        .unwrap_or(1)
}

fn save_parallel_jobs_from_settings(siv: &mut Cursive) {
    let mut raw = String::new();
    siv.call_on_name("settings_parallel_jobs", |v: &mut EditView| {
        raw = v.get_content().to_string();
    });

    let trimmed = raw.trim();
    let Ok(parsed) = trimmed.parse::<usize>() else {
        siv.add_layer(Dialog::info("Parallel jobs must be a positive integer."));
        return;
    };
    if parsed == 0 {
        siv.add_layer(Dialog::info("Parallel jobs must be greater than 0."));
        return;
    }

    if let Some(state) = siv.user_data::<UiState>() {
        state.app_config.parallel_jobs = parsed;
        save_app_config(&state.app_config);
    } else {
        let cfg = AppConfig {
            parallel_jobs: parsed,
            ..AppConfig::default()
        };
        save_app_config(&cfg);
        siv.set_user_data(UiState {
            app_config: cfg,
            running: None,
            exit_notice: None,
            terminal_bg: None,
        });
    }

    siv.pop_layer();
    set_status(siv, format!("Saved settings: parallel_jobs={parsed}"));
}

fn on_parallel_jobs_edit(siv: &mut Cursive, content: &str, _cursor: usize) {
    let sanitized: String = content.chars().filter(|c| c.is_ascii_digit()).collect();
    if sanitized == content {
        return;
    }

    siv.call_on_name("settings_parallel_jobs", |v: &mut EditView| {
        v.set_content(sanitized);
    });
}

fn spawn_toolchain_scan(siv: &mut Cursive) {
    set_status(siv, "Scanning toolchains...");
    let sink = siv.cb_sink().clone();
    std::thread::spawn(move || {
        let found = scan_toolchains(|msg| {
            let _ = sink.send(Box::new(move |siv| {
                set_status(siv, msg.clone());
                append_output(siv, msg);
            }));
        });
        let n = found.len();
        save_config(&found);
        let _ = sink.send(Box::new(move |siv| {
            populate_toolchains(siv, found);
            restore_from_cache(siv);
            set_status(siv, format!("Toolchain scan done: {} found", n));
        }));
    });
}

fn rescan_toolchains(siv: &mut Cursive) {
    spawn_toolchain_scan(siv);
}

fn on_settings(siv: &mut Cursive) {
    let current_jobs = current_parallel_jobs(siv);
    let content = LinearLayout::vertical()
        .child(TextView::new("Parallel build jobs:"))
        .child(
            EditView::new()
                .content(current_jobs.to_string())
                .on_edit(on_parallel_jobs_edit)
                .with_name("settings_parallel_jobs")
                .fixed_width(12),
        );

    let dialog = Dialog::around(content)
        .title("Settings")
        .button("Save", save_parallel_jobs_from_settings)
        .button("Rescan Toolchains", |s| {
            s.pop_layer();
            rescan_toolchains(s);
        })
        .button("Close", |s| {
            s.pop_layer();
        });

    let dialog = OnEventView::new(dialog).on_pre_event(Event::Key(Key::Esc), |s| {
        s.pop_layer();
    });

    siv.add_layer(dialog);
}

fn on_commands(siv: &mut Cursive) {
    let terminal_bg = siv
        .user_data::<UiState>()
        .and_then(|state| state.terminal_bg);
    let command_theme = command_panel_theme(siv.current_theme(), terminal_bg);

    let content = LinearLayout::vertical()
        .child(Button::new("Configure", |s| {
            s.pop_layer();
            on_configure(s);
        }))
        .child(Button::new("Build", |s| {
            s.pop_layer();
            on_build(s);
        }))
        .child(Button::new("Delete and Configure", |s| {
            s.pop_layer();
            on_delete_and_configure(s);
        }))
        .child(Button::new("Clean and Build", |s| {
            s.pop_layer();
            on_clean_and_build(s);
        }));

    let dialog = Layer::new(
        Dialog::around(content)
            .title("Commands")
            .button("Close", |s| {
                s.pop_layer();
            }),
    );

    let dialog = OnEventView::new(dialog).on_pre_event(Event::Key(Key::Esc), |s| {
        s.pop_layer();
    });

    siv.add_layer(ThemedView::new(command_theme, dialog));
}

fn focus_and_open_popup(siv: &mut Cursive, view_name: &str, label: &str) {
    if siv.focus_name(view_name).is_err() {
        set_status(siv, format!("{label} selector is not available"));
        return;
    }
    // Popup SelectView opens on Enter after it gains focus.
    siv.on_event(Event::Key(Key::Enter));
}

fn on_toolchain_hotkey(siv: &mut Cursive) {
    focus_and_open_popup(siv, "toolchain", "Toolchain");
}

fn on_build_type_hotkey(siv: &mut Cursive) {
    focus_and_open_popup(siv, "build_type", "Build type");
}

fn on_target_hotkey(siv: &mut Cursive) {
    focus_and_open_popup(siv, "target", "Target");
}

fn main() {
    reject_startup_if_too_small();

    let terminal_bg = detect_terminal_background_color();
    let mut siv = cursive::default();
    apply_ui_theme(&mut siv, terminal_bg);

    let app_cfg = load_app_config();
    siv.set_user_data(UiState {
        app_config: app_cfg,
        running: None,
        exit_notice: None,
        terminal_bg,
    });

    let project_name = TextView::new("Project: ...")
        .with_name("project_name")
        .full_width();

    let toolchain = SelectView::<Toolchain>::new()
        .popup()
        .with_name("toolchain")
        .full_width();

    let build_type = SelectView::<String>::new()
        .popup()
        .item("Debug", "debug".to_string())
        .item("Release", "release".to_string())
        .item("RelWithDebInfo", "relwithdebinfo".to_string())
        .item("MinSizeRel", "minsizerel".to_string())
        .with_name("build_type")
        .full_width();

    let target = SelectView::<String>::new()
        .popup()
        .with_name("target")
        .full_width();

    let row1 = Panel::new(toolchain).title("Toolchain").full_width();

    let row2 = LinearLayout::horizontal()
        .child(Panel::new(build_type).title("Build Type").full_width())
        .child(DummyView.fixed_width(1))
        .child(Panel::new(target).title("Target").full_width());

    let output = TextView::new("").with_name("output");
    let output_scroll = ScrollView::new(output).with_name("output_scroll");
    let output_panel = Panel::new(output_scroll)
        .title("Build Output")
        .with_name("output_panel")
        .full_height();

    let status = TextView::new("Ready | Waiting for action")
        .no_wrap()
        .with_name("status")
        .full_width();

    let statusbar = LinearLayout::horizontal()
        .child(status)
        .child(DummyView.fixed_width(1))
        .child(Button::new("󰆏", on_copy_output))
        .child(DummyView.fixed_width(1))
        .child(Button::new("󰒓", on_settings))
        .child(DummyView.fixed_width(1))
        .child(Button::new("󰆍", on_commands))
        .child(DummyView.fixed_width(1))
        .child(Button::new("󰈆", |s| s.quit()));

    let status_panel = Panel::new(statusbar).title("Status").full_width();

    let root = LinearLayout::vertical()
        .child(project_name)
        .child(row1)
        .child(row2)
        .child(output_panel)
        .child(status_panel);

    siv.add_fullscreen_layer(root);
    siv.set_fps(4);
    siv.add_global_callback(Event::WindowResize, |s| {
        let h = s.screen_size().y;
        enforce_terminal_height_policy(s, h);
    });
    siv.add_global_callback(Event::Refresh, |s| {
        if let Some(height) = terminal_height() {
            enforce_terminal_height_policy(s, height);
        }
    });
    siv.add_global_callback(Event::Char('g'), on_toolchain_hotkey);
    siv.add_global_callback(Event::Char('b'), on_build_type_hotkey);
    siv.add_global_callback(Event::Char('t'), on_target_hotkey);
    siv.add_global_callback(Event::Char('c'), on_copy_output);
    siv.add_global_callback(Event::Char('s'), on_settings);
    siv.add_global_callback(Event::Char('q'), |s| s.quit());
    siv.add_global_callback(Event::Char('p'), on_commands);

    if let Some(height) = terminal_height() {
        enforce_terminal_height_policy(&mut siv, height);
    }

    refresh_project_name_line(&mut siv);
    restore_build_type_from_workspace_history(&mut siv);

    let cached = load_config();
    if cached.is_empty() {
        spawn_toolchain_scan(&mut siv);
    } else {
        let n = cached.len();
        populate_toolchains(&mut siv, cached);
        restore_from_cache(&mut siv);
        set_status(&mut siv, format!("Loaded {} toolchain(s) from config", n));
    }

    let project_dir = std::env::current_dir().unwrap_or_default();
    let targets = scan_targets_from_build(&project_dir.join("build"));
    if targets.is_empty() {
        set_status(&mut siv, "No build targets (run Configure first)");
    } else {
        let n = targets.len();
        populate_targets(&mut siv, targets);
        restore_target_from_workspace_history(&mut siv);
        set_status(&mut siv, format!("Found {} build target(s)", n));
    }

    siv.run();

    let exit_notice = siv
        .user_data::<UiState>()
        .and_then(|state| state.exit_notice.clone());
    if let Some(msg) = exit_notice {
        println!("{msg}");
    }
}
