use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    pub toolset: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ToolchainConfig {
    pub toolchains: Vec<Toolchain>,
}

pub fn config_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("config")
        .join("toolchain.json")
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
    let out = std::process::Command::new(exe).arg("--version").output().ok()?;
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

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let bare = dir.join(name);
        if bare.is_file() {
            return Some(bare);
        }
        let with_exe = dir.join(format!("{name}.exe"));
        if with_exe.is_file() {
            return Some(with_exe);
        }
    }
    None
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
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit()) {
            if let Ok(y) = s[i..i + 4].parse::<u64>() {
                if (2000..=2100).contains(&y) {
                    return Some(y);
                }
            }
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
    let (major, year) = if version_dir.len() == 4
        && version_dir.chars().all(|c| c.is_ascii_digit())
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
                roots.push(VsInstance { root: p1, generator });
                continue;
            }
            let Ok(levels2) = std::fs::read_dir(&p1) else {
                continue;
            };
            for l2 in levels2.flatten() {
                let p2 = l2.path();
                if p2.join("VC").join("Tools").join("MSVC").is_dir() {
                    let generator = generator_from_root(&p2);
                    roots.push(VsInstance { root: p2, generator });
                }
            }
        }
    }
    roots
}

fn vs_instances() -> Vec<VsInstance> {
    let vswhere = Path::new(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
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
        if let Ok(o) = out {
            if o.status.success() {
                if let Ok(entries) = serde_json::from_slice::<Vec<VsWhereEntry>>(&o.stdout) {
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

        if clang.exists() {
            if let Some(version) = version_of(&clang) {
                found.push(Toolchain {
                    id: "clang".to_string(),
                    name: "Clang".to_string(),
                    version,
                    cxx: sibling_exe(&clang, "clang++.exe"),
                    generator: Some("MinGW Makefiles".to_string()),
                    arch: None,
                    toolset: None,
                    path: clang.to_string_lossy().to_string(),
                });
            }
        }
        if clang_cl.exists() {
            if let Some(version) = version_of(&clang_cl) {
                found.push(Toolchain {
                    id: "clang-cl".to_string(),
                    name: "Clang (MSVC CLI)".to_string(),
                    version,
                    cxx: Some(clang_cl.to_string_lossy().to_string()),
                    generator: vs.generator.clone(),
                    arch: Some("x64".to_string()),
                    toolset: Some("ClangCL".to_string()),
                    path: clang_cl.to_string_lossy().to_string(),
                });
            }
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
    for (cand, id, name) in [
        ("gcc", "gcc", "GCC"),
        ("clang", "clang", "Clang"),
        ("clang-cl", "clang-cl", "Clang (MSVC CLI)"),
    ] {
        let Some(path) = find_on_path(cand) else {
            continue;
        };
        let lower = path.to_string_lossy().to_lowercase();
        if lower.contains(".vscode") || lower.contains("extensions") {
            continue;
        }
        if let Some(version) = version_of(&path) {
            let cxx = if cand == "clang-cl" {
                Some(path.to_string_lossy().to_string())
            } else {
                sibling_exe(&path, if cand == "gcc" { "g++.exe" } else { "clang++.exe" })
            };
            let generator = if cand == "clang-cl" {
                Some("Visual Studio 18 2026".to_string())
            } else {
                Some("MinGW Makefiles".to_string())
            };
            let toolset = if cand == "clang-cl" {
                Some("ClangCL".to_string())
            } else {
                None
            };
            let arch = if cand == "clang-cl" {
                Some("x64".to_string())
            } else {
                None
            };
            found.push(Toolchain {
                id: id.to_string(),
                name: name.to_string(),
                version,
                cxx,
                generator,
                arch,
                toolset,
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
    progress("Scanning toolchains: MinGW GCC...".to_string());
    let mut found = probe_mingw();

    progress("Scanning toolchains: LLVM/Clang...".to_string());
    found.extend(probe_llvm());

    progress("Scanning toolchains: MSVC...".to_string());
    found.extend(probe_msvc());

    progress("Scanning toolchains: PATH fallback...".to_string());
    found.extend(probe_path());

    dedup_toolchains(&mut found);

    progress(format!("Toolchain scan done: {} found", found.len()));
    found
}

pub fn compiler_stem(path: &str) -> Option<String> {
    let file = Path::new(path).file_name()?.to_string_lossy().to_lowercase();
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
    matches!(tc.id.as_str(), "msvc" | "clang-cl")
}

fn push_generator(args: &mut Vec<String>, tc: &Toolchain) {
    if let Some(g) = &tc.generator {
        args.push("-G".to_string());
        args.push(g.clone());
    }
}

pub fn configure_compiler_args(tc: &Toolchain) -> Vec<String> {
    let mut args = Vec::new();
    match tc.id.as_str() {
        "msvc" | "clang-cl" => {
            push_generator(&mut args, tc);
            if let Some(a) = &tc.arch {
                args.push("-A".to_string());
                args.push(a.clone());
            }
            if let Some(t) = &tc.toolset {
                args.push("-T".to_string());
                args.push(t.clone());
            }
        }
        "gcc" => {
            push_generator(&mut args, tc);
            args.push(format!("-DCMAKE_C_COMPILER={}", tc.path));
            args.push(format!(
                "-DCMAKE_CXX_COMPILER={}",
                tc.cxx.clone().unwrap_or_else(|| "g++".to_string())
            ));
        }
        "clang" => {
            push_generator(&mut args, tc);
            args.push(format!("-DCMAKE_C_COMPILER={}", tc.path));
            args.push(format!(
                "-DCMAKE_CXX_COMPILER={}",
                tc.cxx.clone().unwrap_or_else(|| "clang++".to_string())
            ));
        }
        _ => {
            if let Some(stem) = compiler_stem(&tc.path) {
                match stem.as_str() {
                    "g++" | "c++" | "clang++" => {
                        args.push(format!("-DCMAKE_CXX_COMPILER={}", tc.path))
                    }
                    "cl" => {
                        args.push(format!("-DCMAKE_C_COMPILER={}", tc.path));
                        args.push(format!("-DCMAKE_CXX_COMPILER={}", tc.path));
                    }
                    _ => {}
                }
            }
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cache_and_compiler() {
        let dir = std::env::temp_dir().join("cmake_tui_test_cache");
        let _ = std::fs::create_dir_all(&dir);
        let cache_file = dir.join("CMakeCache.txt");
        std::fs::write(
            &cache_file,
            "# comment\nCMAKE_BUILD_TYPE:STRING=Release\nCMAKE_CXX_COMPILER:FILEPATH=E:/foo/cl.exe\nCMAKE_GENERATOR:INTERNAL=Ninja\n",
        )
        .unwrap();

        let map = parse_cmake_cache(&cache_file);
        assert_eq!(map.get("CMAKE_BUILD_TYPE"), Some(&"Release".to_string()));
        assert_eq!(map.get("CMAKE_GENERATOR"), Some(&"Ninja".to_string()));

        assert_eq!(compiler_stem("E:/foo/cl.exe"), Some("cl".to_string()));
        assert_eq!(compiler_stem("g++"), Some("g++".to_string()));
        assert_eq!(compiler_stem("/usr/bin/clang++"), Some("clang++".to_string()));
        assert_eq!(compiler_stem("unknown"), Some("unknown".to_string()));

        let gcc = Toolchain {
            id: "gcc".to_string(),
            name: "GCC".to_string(),
            version: "14.2.0".to_string(),
            path: "D:/mingw64/bin/gcc.exe".to_string(),
            cxx: Some("D:/mingw64/bin/g++.exe".to_string()),
            generator: Some("MinGW Makefiles".to_string()),
            arch: None,
            toolset: None,
        };
        assert_eq!(
            configure_compiler_args(&gcc),
            vec![
                "-G".to_string(),
                "MinGW Makefiles".to_string(),
                "-DCMAKE_C_COMPILER=D:/mingw64/bin/gcc.exe".to_string(),
                "-DCMAKE_CXX_COMPILER=D:/mingw64/bin/g++.exe".to_string()
            ]
        );
        assert!(!is_multi_config(&gcc));

        let cl = Toolchain {
            id: "msvc".to_string(),
            name: "MSVC amd64".to_string(),
            version: "14.51.36231".to_string(),
            path: "E:/foo/cl.exe".to_string(),
            cxx: None,
            generator: Some("Visual Studio 18 2026".to_string()),
            arch: Some("x64".to_string()),
            toolset: None,
        };
        assert_eq!(
            configure_compiler_args(&cl),
            vec![
                "-G".to_string(),
                "Visual Studio 18 2026".to_string(),
                "-A".to_string(),
                "x64".to_string()
            ]
        );

        let cl_cl = Toolchain {
            id: "clang-cl".to_string(),
            name: "Clang (MSVC CLI)".to_string(),
            version: "22.1.3".to_string(),
            path: "E:/foo/clang-cl.exe".to_string(),
            cxx: Some("E:/foo/clang-cl.exe".to_string()),
            generator: Some("Visual Studio 18 2026".to_string()),
            arch: Some("x64".to_string()),
            toolset: Some("ClangCL".to_string()),
        };
        assert_eq!(
            configure_compiler_args(&cl_cl),
            vec![
                "-G".to_string(),
                "Visual Studio 18 2026".to_string(),
                "-A".to_string(),
                "x64".to_string(),
                "-T".to_string(),
                "ClangCL".to_string()
            ]
        );
    }

    #[test]
    fn extract_version_works() {
        assert_eq!(extract_version("clang version 22.1.3 (llvm)"), Some("22.1.3".to_string()));
        assert_eq!(
            extract_version("gcc.exe (x86_64-win32-seh-rev1, Built by MinGW-Builds) 14.2.0"),
            Some("14.2.0".to_string())
        );
        assert_eq!(extract_version("no version here"), None);
    }
}
