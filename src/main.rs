use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cursive::traits::{Nameable, Resizable};
use cursive::views::{
    Button, Dialog, LinearLayout, NamedView, Panel, ResizedView, ScrollView, SelectView, TextView,
};
use cursive::{CbSink, Cursive};

use cmake_tui_tool::{
    compiler_stem, configure_compiler_args, is_multi_config, load_config, parse_cmake_cache,
    save_config, scan_toolchains, Toolchain,
};

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
                && value.as_str() == target {
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
                && compiler_stem(&value.path).as_deref() == Some(stem) {
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
                && value.id == id && (arch.is_empty() || value.arch.as_deref() == Some(arch)) {
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
        && !bt.is_empty() {
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
            && let Some(stem) = compiler_stem(compiler) {
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
    siv.call_on_name("target", |v: &mut SelectView<String>| {
        v.clear();
        for t in targets {
            v.add_item(t.clone(), t);
        }
    });
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
    siv.call_on_name(
        "output_scroll",
        |v: &mut ScrollView<ResizedView<NamedView<TextView>>>| {
            v.scroll_to_bottom();
        },
    );
}

fn refresh_output(siv: &mut Cursive, output: &Arc<Mutex<String>>) {
    let text = output.lock().unwrap().clone();
    siv.call_on_name("output", |v: &mut TextView| v.set_content(text));
    siv.call_on_name(
        "output_scroll",
        |v: &mut ScrollView<ResizedView<NamedView<TextView>>>| {
            v.scroll_to_bottom();
        },
    );
}

fn append_line(output: &Arc<Mutex<String>>, line: &str) {
    const MAX_LINES: usize = 200;
    let mut o = output.lock().unwrap();
    o.push_str(line);
    o.push('\n');
    let newlines = o.bytes().filter(|&b| b == b'\n').count();
    if newlines > MAX_LINES {
        let skip = newlines - MAX_LINES;
        let mut pos = 0;
        let mut seen = 0;
        for (i, b) in o.bytes().enumerate() {
            if b == b'\n' {
                seen += 1;
                if seen == skip {
                    pos = i + 1;
                    break;
                }
            }
        }
        let tail = o[pos..].to_string();
        *o = tail;
    }
}

fn maybe_refresh(output: &Arc<Mutex<String>>, sink: &CbSink, last: &Arc<Mutex<Instant>>) {
    let now = Instant::now();
    let due = {
        let mut l = last.lock().unwrap();
        if now.duration_since(*l) >= Duration::from_millis(100) {
            *l = now;
            true
        } else {
            false
        }
    };
    if due {
        let output = Arc::clone(output);
        let sink = sink.clone();
        let _ = sink.send(Box::new(move |siv| refresh_output(siv, &output)));
    }
}

fn read_lines<R: std::io::Read>(
    reader: R,
    output: &Arc<Mutex<String>>,
    sink: &CbSink,
    last: &Arc<Mutex<Instant>>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\r', '\n']);
                append_line(output, line);
                maybe_refresh(output, sink, last);
            }
            Err(_) => break,
        }
    }
}

fn run_cmake_command(sink: &CbSink, mut cmd: std::process::Command, header: &str) -> bool {
    let output = Arc::new(Mutex::new(String::new()));
    let last = Arc::new(Mutex::new(Instant::now()));

    if !header.is_empty() {
        append_line(&output, header);
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to start cmake: {}", e);
            let sink = sink.clone();
            let _ = sink.send(Box::new(move |siv| set_status(siv, msg)));
            return false;
        }
    };

    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let sink = sink.clone();
        let output = Arc::clone(&output);
        let last = Arc::clone(&last);
        readers.push(std::thread::spawn(move || {
            read_lines(stdout, &output, &sink, &last);
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let sink = sink.clone();
        let output = Arc::clone(&output);
        let last = Arc::clone(&last);
        readers.push(std::thread::spawn(move || {
            read_lines(stderr, &output, &sink, &last);
        }));
    }

    let status = child.wait();
    for reader in readers {
        let _ = reader.join();
    }
    let ok = status.map(|s| s.success()).unwrap_or(false);

    let output = Arc::clone(&output);
    let sink = sink.clone();
    let _ = sink.send(Box::new(move |siv| refresh_output(siv, &output)));

    ok
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
        && cache.get("CMAKE_GENERATOR_PLATFORM").is_some_and(|p| p != arch) {
            return Some(format!(
                "platform: {} -> {}",
                cache.get("CMAKE_GENERATOR_PLATFORM").unwrap(),
                arch
            ));
        }
    if let Some(toolset) = &toolchain.toolset
        && cache.get("CMAKE_GENERATOR_TOOLSET").is_some_and(|t| t != toolset) {
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

fn on_configure(siv: &mut Cursive) {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let build_type = selection(siv, "build_type");
    let Some(toolchain) = selected_toolchain(siv) else {
        set_status(siv, "No toolchain selected");
        return;
    };
    let toolchain_label = format!("{} ({})", toolchain.name, toolchain.version);
    let sink = siv.cb_sink().clone();

    clear_output(siv);
    set_status(
        siv,
        format!("Configuring: toolchain={} build_type={}", toolchain_label, build_type),
    );

    std::thread::spawn(move || {
        let build_dir = project_dir.join("build");

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
            let sink = sink.clone();
            let _ = sink.send(Box::new(move |siv| append_output(siv, msg)));
            clear_build_cache(&build_dir);
        } else if broken_cache(&build_dir) {
            let msg = "Broken build cache detected (compiler/linker NOTFOUND), clearing it";
            let sink = sink.clone();
            let _ = sink.send(Box::new(move |siv| append_output(siv, msg)));
            clear_build_cache(&build_dir);
        }

        let cmdline = format_command(&args);
        let header = format!("{}\n$ {cmdline}", env_diagnostics());

        let mut cmd = std::process::Command::new("cmake");
        cmd.args(&args);
        clean_cmake_env(&mut cmd);

        let ok = run_cmake_command(&sink, cmd, &header);

        if ok {
            let targets = scan_targets_from_build(&build_dir);
            let n = targets.len();
            let _ = sink.send(Box::new(move |siv| {
                populate_targets(siv, targets);
                set_status(siv, format!("Configure succeeded: {} target(s)", n));
            }));
        } else {
            let _ = sink.send(Box::new(move |siv| set_status(siv, "Configure failed")));
        }
    });
}

fn on_build(siv: &mut Cursive) {
    let project_dir = std::env::current_dir().unwrap_or_default();
    let target = selection(siv, "target");
    let build_type = selection(siv, "build_type");
    let sink = siv.cb_sink().clone();

    let label = if target.is_empty() {
        "all".to_string()
    } else {
        target.clone()
    };
    clear_output(siv);
    set_status(siv, format!("Building '{}'...", label));

    std::thread::spawn(move || {
        let build_dir = project_dir.join("build");
        let cache = parse_cmake_cache(&build_dir.join("CMakeCache.txt"));
        let is_vs = cache
            .get("CMAKE_GENERATOR")
            .is_some_and(|g| g.contains("Visual Studio"));

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
        clean_cmake_env(&mut cmd);

        let ok = run_cmake_command(&sink, cmd, &format!("$ cmake --build {}", build_dir.to_string_lossy()));

        let _ = sink.send(Box::new(move |siv| {
            if ok {
                set_status(siv, format!("Build succeeded: {}", label));
            } else {
                set_status(siv, format!("Build failed: {}", label));
            }
        }));
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
    siv.add_layer(
        Dialog::text("Settings")
            .button("Rescan Toolchains", |s| {
                s.pop_layer();
                rescan_toolchains(s);
            })
            .button("Close", |s| {
                s.pop_layer();
            }),
    );
}

fn main() {
    let mut siv = cursive::default();

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
        .child(Panel::new(target).title("Target").full_width());

    let actions = LinearLayout::horizontal()
        .child(Button::new("Configure", on_configure))
        .child(Button::new("Build", on_build));

    let output = TextView::new("").with_name("output").full_width();
    let output_scroll = ScrollView::new(output)
        .with_name("output_scroll")
        .full_height();

    let status = TextView::new("Ready")
        .no_wrap()
        .with_name("status")
        .full_width();

    let statusbar = LinearLayout::horizontal()
        .child(status)
        .child(Button::new("Copy Output", on_copy_output))
        .child(Button::new("Settings", on_settings));

    let root = LinearLayout::vertical()
        .child(row1)
        .child(row2)
        .child(actions)
        .child(output_scroll)
        .child(statusbar);

    siv.add_layer(root);

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
        set_status(&mut siv, format!("Found {} build target(s)", n));
    }

    siv.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_help() {
        let output = "[1/1] All primary targets available:\n\
edit_cache: phony\n\
install: phony\n\
Mp4Parser-BC: phony\n\
ExtractVideoStream: phony\n\
ExtractVideoStream.exe: phony\n\
Mp4ParseLib/samples/edit_cache: phony\n\
build.ninja: RERUN_CMAKE\n\
clean: CLEAN\n\
help: HELP\n";
        let targets = parse_cmake_target_help(output);
        assert_eq!(
            targets,
            vec![
                "install".to_string(),
                "Mp4Parser-BC".to_string(),
                "ExtractVideoStream".to_string()
            ]
        );
    }

    #[test]
    fn parse_makefiles_target_help() {
        let output = "The following are some of the valid targets for this Makefile:\n\
... all (the default if no target is provided)\n\
... clean\n\
... depend\n\
... edit_cache\n\
... install\n\
... install/local\n\
... install/strip\n\
... list_install_components\n\
... rebuild_cache\n\
... ExtractVideoStream\n\
... Mp4InfoDisplay\n\
... Mp4ParseLib\n\
... Mp4Parser-BC\n\
... ParseMp4Files\n\
... imgui\n\
... resources/app.obj\n\
... src/Mp4ParseData.obj\n";
        let targets = parse_cmake_target_help(output);
        assert_eq!(
            targets,
            vec![
                "all".to_string(),
                "install".to_string(),
                "ExtractVideoStream".to_string(),
                "Mp4InfoDisplay".to_string(),
                "Mp4ParseLib".to_string(),
                "Mp4Parser-BC".to_string(),
                "ParseMp4Files".to_string(),
                "imgui".to_string()
            ]
        );
    }

    #[test]
    fn scan_targets_from_slnx_parses() {
        let dir = std::env::temp_dir().join("cmake_tui_test_slnx");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Test.slnx"),
            "<?xml version=\"1.0\"?>\n<Solution>\n  <Project Path=\"ALL_BUILD.vcxproj\" Type=\"8bc9ceb8\"/>\n  <Project Path=\"INSTALL.vcxproj\" Type=\"8bc9ceb8\"/>\n  <Project Path=\"Mp4ParseLib/samples/ExtractVideoStream.vcxproj\" Type=\"8bc9ceb8\"/>\n  <Project Path=\"Mp4Parser-BC.vcxproj\" Type=\"8bc9ceb8\"/>\n  <Project Path=\"ZERO_CHECK.vcxproj\" Type=\"8bc9ceb8\"/>\n</Solution>\n",
        )
        .unwrap();
        let targets = scan_targets_from_slnx(&dir);
        assert_eq!(
            targets,
            vec![
                "all".to_string(),
                "install".to_string(),
                "ExtractVideoStream".to_string(),
                "Mp4Parser-BC".to_string()
            ]
        );
    }
}
