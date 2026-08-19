use std::collections::BTreeSet;

use cmake_tui_tool::scan_toolchains;
use serde::Deserialize;

#[derive(Deserialize)]
struct Kit {
    name: String,
    #[serde(default)]
    compilers: Option<KitCompilers>,
}

#[derive(Deserialize)]
struct KitCompilers {
    #[serde(rename = "C", default)]
    c: Option<String>,
    #[serde(rename = "CXX", default)]
    cxx: Option<String>,
}

const DEFAULT_KITS: &str = r"C:\Users\56944\AppData\Local\CMakeTools\cmake-tools-kits.json";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(|s| s.as_str()).unwrap_or("scan");
    match mode {
        "benchmark" | "bench" => benchmark(&args),
        _ => scan(&args),
    }
}

fn scan(args: &[String]) {
    let json = args.contains(&"--json".to_string());
    let found = scan_toolchains(|msg| eprintln!("{msg}"));
    if json {
        println!("{}", serde_json::to_string_pretty(&found).unwrap());
    } else {
        for t in &found {
            let mut extra = String::new();
            if let Some(g) = &t.generator {
                extra.push_str(&format!("  gen={g}"));
            }
            if let Some(a) = &t.arch {
                extra.push_str(&format!("  arch={a}"));
            }
            if let Some(tri) = &t.triple {
                extra.push_str(&format!("  triple={tri}"));
            }
            if let Some(tool) = &t.toolset {
                extra.push_str(&format!("  toolset={tool}"));
            }
            match &t.cxx {
                Some(cxx) => println!("[{:8}] {:16} {:12} {}  (cxx: {}){}", t.id, t.name, t.version, t.path, cxx, extra),
                None => println!("[{:8}] {:16} {:12} {}{}", t.id, t.name, t.version, t.path, extra),
            }
        }
        println!("\n{} toolchain(s) found", found.len());
    }
}

fn kits_path_from_args(args: &[String]) -> String {
    for (i, a) in args.iter().enumerate() {
        if a == "--kits"
            && let Some(p) = args.get(i + 1) {
                return p.clone();
            }
    }
    DEFAULT_KITS.to_string()
}

fn family_of_kit(name: &str) -> &'static str {
    if name.contains("Clang") {
        "clang"
    } else if name.contains("GCC") {
        "gcc"
    } else if name.contains("Visual Studio") {
        "msvc"
    } else {
        "other"
    }
}

fn load_kits(path: &str) -> Vec<Kit> {
    let s = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read kits file '{}': {}", path, e);
            std::process::exit(2);
        }
    };
    serde_json::from_str(&s).unwrap_or_else(|e| {
        eprintln!("Failed to parse kits file '{}': {}", path, e);
        std::process::exit(2);
    })
}

fn benchmark(args: &[String]) {
    let kits_path = kits_path_from_args(args);
    let kits = load_kits(&kits_path);
    let found = scan_toolchains(|msg| eprintln!("{msg}"));

    // Reference compiler paths (unique, lowercased).
    let mut ref_paths: BTreeSet<String> = BTreeSet::new();
    for k in &kits {
        if let Some(c) = &k.compilers {
            if let Some(p) = &c.c {
                ref_paths.insert(p.to_lowercase());
            }
            if let Some(p) = &c.cxx {
                ref_paths.insert(p.to_lowercase());
            }
        }
    }

    // Scanned compiler paths (unique, lowercased).
    let mut scan_paths: BTreeSet<String> = BTreeSet::new();
    for t in &found {
        scan_paths.insert(t.path.to_lowercase());
        if let Some(c) = &t.cxx {
            scan_paths.insert(c.to_lowercase());
        }
    }

    let matched: Vec<&String> = ref_paths.iter().filter(|p| scan_paths.contains(*p)).collect();
    let missing: Vec<&String> = ref_paths.iter().filter(|p| !scan_paths.contains(*p)).collect();
    let extra: Vec<&String> = scan_paths.iter().filter(|p| !ref_paths.contains(*p)).collect();

    // MSVC host/target combos.
    let ref_msvc: BTreeSet<String> = kits
        .iter()
        .filter(|k| family_of_kit(&k.name) == "msvc")
        .filter_map(|k| k.name.split(" - ").nth(1).map(|s| s.to_string()))
        .collect();
    let scan_msvc: BTreeSet<String> = found
        .iter()
        .filter(|t| t.id == "msvc")
        .filter_map(|t| t.name.strip_prefix("MSVC ").map(|s| s.to_string()))
        .collect();

    println!("\n=== Toolchain scan results ===");
    for t in &found {
        println!("  [{:8}] {:20} {:12} {}", t.id, t.name, t.version, t.path);
        if let Some(cxx) = &t.cxx {
            println!("            cxx: {}", cxx);
        }
    }

    println!("\n=== Benchmark vs {} ===", kits_path);
    println!("reference kits: {} (gcc={}, clang={}, msvc={})",
        kits.len(),
        kits.iter().filter(|k| family_of_kit(&k.name) == "gcc").count(),
        kits.iter().filter(|k| family_of_kit(&k.name) == "clang").count(),
        kits.iter().filter(|k| family_of_kit(&k.name) == "msvc").count(),
    );

    println!("\n[compiler paths] reference={} scanned={}", ref_paths.len(), scan_paths.len());
    println!("  matched: {}", matched.len());
    for p in &matched {
        println!("    OK   {}", p);
    }
    if !missing.is_empty() {
        println!("  missing: {}", missing.len());
        for p in &missing {
            println!("    MISS {}", p);
        }
    }
    if !extra.is_empty() {
        println!("  extra (not in reference): {}", extra.len());
        for p in &extra {
            println!("    EXTRA {}", p);
        }
        println!("    note: clang++.exe is used as the C++ driver for the GNU-CLI clang");
        println!("          (CMake Tools lists clang.exe for both C and CXX); MSVC cl.exe");
        println!("          paths are implicit in the reference VS kits, so they appear extra.");
    }

    println!("\n[MSVC host/target combos] reference={} scanned={}", ref_msvc.len(), scan_msvc.len());
    let ms_match = ref_msvc.intersection(&scan_msvc).count();
    println!("  matched combos: {}", ms_match);
    if !ref_msvc.is_empty() {
        for c in &ref_msvc {
            let ok = if scan_msvc.contains(c) { "OK  " } else { "MISS" };
            println!("    {} {}", ok, c);
        }
    }

    let total = ref_paths.len();
    if total > 0 {
        println!("\n=== Coverage: {}/{} unique reference compiler paths ===", matched.len(), total);
    }
}
