use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod runtime;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Toolchain {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: String,
    #[serde(default)]
    pub cxx: Option<String>,
    #[serde(default)]
    pub generator: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub triple: Option<String>,
    #[serde(default)]
    pub toolset: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ToolchainConfig {
    pub toolchains: Vec<Toolchain>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    pub parallel_jobs: usize,
    #[serde(default = "default_workspace_history_limit")]
    pub workspace_history_limit: usize,
    #[serde(default)]
    pub workspace_builds: HashMap<String, WorkspaceBuildConfig>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WorkspaceBuildConfig {
    #[serde(default)]
    pub build_type: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub last_used_unix_secs: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            parallel_jobs: host_core_count(),
            workspace_history_limit: default_workspace_history_limit(),
            workspace_builds: HashMap::new(),
        }
    }
}

fn default_workspace_history_limit() -> usize {
    200
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn workspace_key(workspace_dir: &Path) -> String {
    let raw = workspace_dir.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        raw.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        raw
    }
}

impl AppConfig {
    fn prune_workspace_builds(&mut self) {
        let limit = self.workspace_history_limit.max(1);
        while self.workspace_builds.len() > limit {
            let oldest_key = self
                .workspace_builds
                .iter()
                .min_by_key(|(_, v)| v.last_used_unix_secs)
                .map(|(k, _)| k.clone());
            let Some(key) = oldest_key else {
                break;
            };
            self.workspace_builds.remove(&key);
        }
    }

    pub fn remember_workspace_build(
        &mut self,
        workspace_dir: &Path,
        build_type: &str,
        target: &str,
    ) {
        let key = workspace_key(workspace_dir);
        self.workspace_builds.insert(
            key,
            WorkspaceBuildConfig {
                build_type: build_type.to_string(),
                target: target.to_string(),
                last_used_unix_secs: unix_now_secs(),
            },
        );
        self.prune_workspace_builds();
    }

    pub fn workspace_build(&self, workspace_dir: &Path) -> Option<&WorkspaceBuildConfig> {
        let key = workspace_key(workspace_dir);
        self.workspace_builds.get(&key)
    }

    pub fn normalize(&mut self) {
        if self.parallel_jobs == 0 {
            self.parallel_jobs = host_core_count();
        }
        if self.workspace_history_limit == 0 {
            self.workspace_history_limit = default_workspace_history_limit();
        }
        self.prune_workspace_builds();
    }
}

fn host_core_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn config_dir() -> PathBuf {
    if let Some(base) = dirs::config_dir() {
        return base.join("cmake-tui-tool");
    }

    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("config")
}

pub fn app_config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn config_path() -> PathBuf {
    config_dir().join("toolchain.json")
}

pub fn load_app_config() -> AppConfig {
    let path = app_config_path();
    let loaded = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<AppConfig>(&s).ok(),
        Err(_) => None,
    };

    let mut cfg = loaded.unwrap_or_default();
    cfg.normalize();

    if !path.exists() {
        save_app_config(&cfg);
    }

    cfg
}

pub fn save_app_config(cfg: &AppConfig) {
    let path = app_config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, s);
    }
}

pub fn load_config() -> Vec<Toolchain> {
    match std::fs::read_to_string(config_path()) {
        Ok(s) => serde_json::from_str::<ToolchainConfig>(&s)
            .map(|c| c.toolchains)
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save_config(toolchains: &[Toolchain]) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let cfg = ToolchainConfig {
        toolchains: toolchains.to_vec(),
    };
    if let Ok(s) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(path, s);
    }
}

pub fn extract_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let cand = text[start..i].trim_end_matches('.');
            let parts: Vec<&str> = cand.split('.').collect();
            if parts.len() >= 3
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                return Some(parts[..3].join("."));
            }
        } else {
            i += 1;
        }
    }
    None
}

fn version_of(exe: &Path) -> Option<String> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .output()
        .ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    extract_version(&text)
}

fn sibling_exe(exe: &Path, name: &str) -> Option<String> {
    let p = exe.parent()?.join(name);
    p.exists().then(|| p.to_string_lossy().to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum Vendor {
    Gcc,
    Clang,
    ClangCl,
}

fn compiler_kind(fname: &str) -> Option<Vendor> {
    let stem = fname.trim_end_matches(".exe").to_lowercase();
    if stem == "clang-cl" || stem.starts_with("clang-cl-") {
        return Some(Vendor::ClangCl);
    }
    if stem == "clang" || stem.starts_with("clang-") {
        return Some(Vendor::Clang);
    }
    if stem == "gcc" || stem.starts_with("gcc-") || stem.ends_with("-gcc") || stem.contains("-gcc-")
    {
        return Some(Vendor::Gcc);
    }
    None
}

fn first_version_token(s: &str) -> Option<String> {
    let t = s.split_whitespace().next()?.trim_end_matches('.');
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn clang_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim_start();
        let rest = if let Some(i) = line.find("clang version ") {
            &line[i + "clang version ".len()..]
        } else if let Some(i) = line.find("Apple LLVM version ") {
            &line[i + "Apple LLVM version ".len()..]
        } else {
            continue;
        };
        if let Some(v) = first_version_token(rest) {
            return Some(v);
        }
    }
    None
}

fn gcc_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim_start();
        let rest = if let Some(r) = line.strip_prefix("gcc version ") {
            r
        } else if let Some(r) = line.strip_prefix("gcc-version ") {
            r
        } else {
            continue;
        };
        if let Some(v) = first_version_token(rest) {
            return Some(v);
        }
    }
    None
}

fn target_triple(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Target: ") {
            let t = rest.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn detect_compiler(exe: &Path, vendor: Vendor) -> Option<(String, Option<String>)> {
    let out = std::process::Command::new(exe).arg("-v").output().ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let version = match vendor {
        Vendor::Clang | Vendor::ClangCl => clang_version(&text)?,
        Vendor::Gcc => gcc_version(&text)?,
    };
    Some((version, target_triple(&text)))
}

fn sibling_compiler(exe: &Path, from: &str, to: &str) -> Option<String> {
    let fname = exe.file_name()?.to_string_lossy().to_string();
    let new_fname = fname.replacen(from, to, 1);
    if new_fname == fname {
        return None;
    }
    let p = exe.parent()?.join(new_fname);
    p.exists().then(|| p.to_string_lossy().to_string())
}

fn arch_label(a: &str) -> &str {
    match a {
        "x64" => "amd64",
        "x86" => "x86",
        "arm64" => "arm64",
        _ => a,
    }
}

fn host_label(host: &str) -> String {
    let lower = host.to_lowercase();
    let rest = lower.strip_prefix("host").unwrap_or(&lower);
    arch_label(rest).to_string()
}

fn msvc_name(host: &str, target: &str) -> String {
    let h = host_label(host);
    let t = arch_label(&target.to_lowercase()).to_string();
    if h == t {
        format!("MSVC {h}")
    } else {
        format!("MSVC {h}_{t}")
    }
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

struct VsInstance {
    root: PathBuf,
    generator: Option<String>,
}

#[derive(Deserialize)]
struct VsWhereEntry {
    #[serde(rename = "installationPath")]
    path: String,
    #[serde(rename = "installationVersion", default)]
    version: Option<String>,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
}

fn year_for_major(major: u64) -> u64 {
    match major {
        15 => 2017,
        16 => 2019,
        17 => 2022,
        18 => 2026,
        19 => 2029,
        _ => 2008 + major,
    }
}

fn extract_year(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    for i in 0..bytes.len().saturating_sub(3) {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit())
            && let Ok(y) = s[i..i + 4].parse::<u64>()
            && (2000..=2100).contains(&y)
        {
            return Some(y);
        }
    }
    None
}

fn vs_generator(version: &str, display_name: &str) -> Option<String> {
    let major = version.split('.').next()?.parse::<u64>().ok()?;
    let year = extract_year(display_name).unwrap_or_else(|| year_for_major(major));
    Some(format!("Visual Studio {major} {year}"))
}

fn generator_from_root(root: &Path) -> Option<String> {
    let version_dir = root.parent()?.file_name()?.to_string_lossy().to_string();
    let (major, year) = if version_dir.len() == 4 && version_dir.chars().all(|c| c.is_ascii_digit())
    {
        let year: u64 = version_dir.parse().ok()?;
        let major = match year {
            2017 => 15,
            2019 => 16,
            2022 => 17,
            2026 => 18,
            2029 => 19,
            _ => return None,
        };
        (major, year)
    } else {
        let major: u64 = version_dir.parse().ok()?;
        (major, year_for_major(major))
    };
    Some(format!("Visual Studio {major} {year}"))
}

fn discover_vs_roots() -> Vec<VsInstance> {
    let mut roots = Vec::new();
    for base in [
        r"C:\Program Files\Microsoft Visual Studio",
        r"C:\Program Files (x86)\Microsoft Visual Studio",
    ] {
        let Ok(levels1) = std::fs::read_dir(base) else {
            continue;
        };
        for l1 in levels1.flatten() {
            let p1 = l1.path();
            if !p1.is_dir() {
                continue;
            }
            if p1.join("VC").join("Tools").join("MSVC").is_dir() {
                let generator = generator_from_root(&p1);
                roots.push(VsInstance {
                    root: p1,
                    generator,
                });
                continue;
            }
            let Ok(levels2) = std::fs::read_dir(&p1) else {
                continue;
            };
            for l2 in levels2.flatten() {
                let p2 = l2.path();
                if p2.join("VC").join("Tools").join("MSVC").is_dir() {
                    let generator = generator_from_root(&p2);
                    roots.push(VsInstance {
                        root: p2,
                        generator,
                    });
                }
            }
        }
    }
    roots
}

fn vs_instances() -> Vec<VsInstance> {
    let vswhere =
        Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if vswhere.exists() {
        let out = std::process::Command::new(vswhere)
            .args([
                "-products",
                "*",
                "-requires",
                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-format",
                "json",
            ])
            .output();
        if let Ok(o) = out
            && o.status.success()
            && let Ok(entries) = serde_json::from_slice::<Vec<VsWhereEntry>>(&o.stdout)
        {
            let instances: Vec<VsInstance> = entries
                .into_iter()
                .filter(|e| !e.path.is_empty())
                .map(|e| {
                    let generator = e
                        .version
                        .as_deref()
                        .and_then(|v| vs_generator(v, e.display_name.as_deref().unwrap_or("")));
                    VsInstance {
                        root: PathBuf::from(e.path),
                        generator,
                    }
                })
                .collect();
            if !instances.is_empty() {
                return instances;
            }
        }
    }
    discover_vs_roots()
}

pub fn probe_mingw() -> Vec<Toolchain> {
    let roots = [
        r"D:\mingw64",
        r"C:\mingw64",
        r"C:\msys64\mingw64",
        r"C:\msys64\ucrt64",
        r"C:\msys64\clang64",
    ];
    let mut found = Vec::new();
    for root in roots {
        let gcc = Path::new(root).join("bin").join("gcc.exe");
        if !gcc.exists() {
            continue;
        }
        if let Some(version) = version_of(&gcc) {
            found.push(Toolchain {
                id: "gcc".to_string(),
                name: "GCC".to_string(),
                version,
                cxx: sibling_exe(&gcc, "g++.exe"),
                generator: Some("MinGW Makefiles".to_string()),
                arch: None,
                triple: None,
                toolset: None,
                path: gcc.to_string_lossy().to_string(),
            });
        }
    }
    found
}

fn cmake_arch(target: &str) -> &str {
    match target.to_lowercase().as_str() {
        "x64" => "x64",
        "x86" => "Win32",
        "arm64" => "ARM64",
        _ => target,
    }
}

fn host_toolset(host: &str) -> Option<String> {
    host.to_lowercase()
        .contains("x86")
        .then(|| "host=x86".to_string())
}

pub fn probe_llvm() -> Vec<Toolchain> {
    let mut found = Vec::new();
    for vs in vs_instances() {
        let bin = vs
            .root
            .join("VC")
            .join("Tools")
            .join("Llvm")
            .join("x64")
            .join("bin");
        let clang = bin.join("clang.exe");
        let clang_cl = bin.join("clang-cl.exe");

        if clang.exists()
            && let Some(version) = version_of(&clang)
        {
            found.push(Toolchain {
                id: "clang".to_string(),
                name: "Clang".to_string(),
                version,
                cxx: sibling_exe(&clang, "clang++.exe"),
                generator: Some("MinGW Makefiles".to_string()),
                arch: None,
                triple: None,
                toolset: None,
                path: clang.to_string_lossy().to_string(),
            });
        }
        if clang_cl.exists()
            && let Some(version) = version_of(&clang_cl)
        {
            found.push(Toolchain {
                id: "clang-cl".to_string(),
                name: "Clang (MSVC CLI)".to_string(),
                version,
                cxx: Some(clang_cl.to_string_lossy().to_string()),
                generator: vs.generator.clone(),
                arch: Some("x64".to_string()),
                triple: None,
                toolset: Some("ClangCL".to_string()),
                path: clang_cl.to_string_lossy().to_string(),
            });
        }
    }
    found
}

pub fn probe_msvc() -> Vec<Toolchain> {
    let mut found = Vec::new();
    for vs in vs_instances() {
        let msvc = vs.root.join("VC").join("Tools").join("MSVC");
        let Ok(entries) = std::fs::read_dir(&msvc) else {
            continue;
        };

        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| {
                p.file_name()
                    .map(|n| {
                        n.to_string_lossy()
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false)
            })
            .collect();
        versions.sort_by_key(|p| {
            version_key(
                &p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            )
        });

        let Some(latest) = versions.last() else {
            continue;
        };
        let version = latest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let Ok(hosts) = std::fs::read_dir(latest.join("bin")) else {
            continue;
        };
        for host in hosts.flatten() {
            let host_path = host.path();
            if !host_path.is_dir() {
                continue;
            }
            let host_name = host_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(targets) = std::fs::read_dir(&host_path) else {
                continue;
            };
            for target in targets.flatten() {
                let target_path = target.path();
                if !target_path.is_dir() {
                    continue;
                }
                let cl = target_path.join("cl.exe");
                if !cl.exists() {
                    continue;
                }
                let target_name = target_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                found.push(Toolchain {
                    id: "msvc".to_string(),
                    name: msvc_name(&host_name, &target_name),
                    version: version.clone(),
                    cxx: None,
                    generator: vs.generator.clone(),
                    arch: Some(cmake_arch(&target_name).to_string()),
                    triple: None,
                    toolset: host_toolset(&host_name),
                    path: cl.to_string_lossy().to_string(),
                });
            }
        }
    }
    found
}

fn probe_path() -> Vec<Toolchain> {
    let mut found = Vec::new();
    let Some(path_var) = std::env::var_os("PATH") else {
        return found;
    };
    for dir in std::env::split_paths(&path_var) {
        let dir_str = dir.to_string_lossy().to_lowercase();
        if dir_str.contains(".vscode") || dir_str.contains("extensions") {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(fname) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            let Some(vendor) = compiler_kind(&fname) else {
                continue;
            };
            let Some((version, triple)) = detect_compiler(&path, vendor) else {
                continue;
            };

            let (id, name) = match vendor {
                Vendor::Gcc => ("gcc", "GCC"),
                Vendor::Clang => ("clang", "Clang"),
                Vendor::ClangCl => ("clang-cl", "Clang-cl"),
            };

            let cxx = match vendor {
                Vendor::ClangCl => Some(path.to_string_lossy().to_string()),
                Vendor::Gcc => sibling_compiler(&path, "gcc", "g++"),
                Vendor::Clang => sibling_compiler(&path, "clang", "clang++"),
            };

            let mingw = matches!(vendor, Vendor::Gcc | Vendor::Clang) && cfg!(windows) && {
                let lp = path.to_string_lossy().to_lowercase();
                (lp.contains("mingw") || lp.contains("msys"))
                    && path
                        .parent()
                        .is_some_and(|d| d.join("mingw32-make.exe").exists())
            };
            let generator = if mingw {
                Some("MinGW Makefiles".to_string())
            } else {
                None
            };

            found.push(Toolchain {
                id: id.to_string(),
                name: name.to_string(),
                version,
                cxx,
                generator,
                arch: None,
                triple,
                toolset: None,
                path: path.to_string_lossy().to_string(),
            });
        }
    }
    found
}

fn dedup_toolchains(list: &mut Vec<Toolchain>) {
    let mut seen = HashSet::new();
    list.retain(|tc| seen.insert(tc.path.to_lowercase()));
}

pub fn scan_toolchains(mut progress: impl FnMut(String)) -> Vec<Toolchain> {
    let mut found = Vec::new();

    if cfg!(windows) {
        progress("Scanning toolchains: MinGW GCC...".to_string());
        found.extend(probe_mingw());

        progress("Scanning toolchains: LLVM/Clang...".to_string());
        found.extend(probe_llvm());

        progress("Scanning toolchains: MSVC...".to_string());
        found.extend(probe_msvc());
    }

    progress("Scanning toolchains: PATH fallback...".to_string());
    found.extend(probe_path());

    dedup_toolchains(&mut found);

    progress(format!("Toolchain scan done: {} found", found.len()));
    found
}

pub fn compiler_stem(path: &str) -> Option<String> {
    let file = Path::new(path)
        .file_name()?
        .to_string_lossy()
        .to_lowercase();
    Some(file.trim_end_matches(".exe").to_string())
}

pub fn parse_cmake_cache(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return map,
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let key_part = &line[..eq];
        let value = &line[eq + 1..];
        if let Some(colon) = key_part.find(':') {
            let key = key_part[..colon].trim();
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

pub fn is_multi_config(tc: &Toolchain) -> bool {
    tc.generator
        .as_deref()
        .is_some_and(|g| g.starts_with("Visual Studio"))
}

fn push_generator(args: &mut Vec<String>, tc: &Toolchain) {
    if let Some(g) = &tc.generator {
        args.push("-G".to_string());
        args.push(g.clone());
    }
}

pub fn configure_compiler_args(tc: &Toolchain) -> Vec<String> {
    let mut args = Vec::new();
    let vs = tc
        .generator
        .as_deref()
        .is_some_and(|g| g.starts_with("Visual Studio"));

    if vs {
        push_generator(&mut args, tc);
        if let Some(a) = &tc.arch {
            args.push("-A".to_string());
            args.push(a.clone());
        }
        if let Some(t) = &tc.toolset {
            args.push("-T".to_string());
            args.push(t.clone());
        }
        return args;
    }

    push_generator(&mut args, tc);
    args.push(format!("-DCMAKE_C_COMPILER={}", tc.path));
    let cxx = tc
        .cxx
        .clone()
        .unwrap_or_else(|| match compiler_stem(&tc.path).as_deref() {
            Some("gcc") => "g++".to_string(),
            _ => "clang++".to_string(),
        });
    args.push(format!("-DCMAKE_CXX_COMPILER={}", cxx));
    args
}
