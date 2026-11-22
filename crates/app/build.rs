#[path = "src/tray/icon_art.rs"]
mod icon_art;
#[path = "src/release_number.rs"]
mod release_number;

use std::path::Path;

fn main() {
    // The UI (ui/*.slint) is compiled by crates/ui, not here.
    let version = app_version();
    println!("cargo:rustc-env=APP_VERSION={}", version.text);

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let sources = ["src/platform/macos/artwork.m", "src/platform/macos/spectrum.m", "src/platform/macos/particles.m", "src/platform/macos/transport.m"];
        let mut build = cc::Build::new();
        if std::env::var_os("CARGO_FEATURE_BENCH").is_some() {
            build.define("MP_BENCH", None);
        }
        build.files(sources).flag("-fobjc-arc").compile("marquee_appkit");
        for source in sources {
            println!("cargo:rerun-if-changed={source}");
        }
        println!("cargo:rustc-link-lib=framework=MediaPlayer");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=CoreImage");
        println!("cargo:rerun-if-changed=src/platform/macos/particles.h");
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if msvc {
        // The generated UI builds large structures on the main thread's stack; Windows'
        // default 1 MB is not enough for this window. 8 MB.
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");

        // The app icon and the version as Win32 resources, so Explorer, the taskbar and
        // the title bar show the icon, and Properties > Details the version. link.exe
        // takes a compiled .res file directly, so no rc.exe is needed.
        let out = std::env::var("OUT_DIR").unwrap();
        let res = Path::new(&out).join("app-icon.res");
        let mut data = icon_res(&[16, 20, 24, 32, 40, 48, 64, 96, 128, 256]);
        res_entry(&mut data, RT_VERSION, 1, 0x0030, &version_info(&version));
        // The application manifest: runs as the user (never asks for elevation), declares
        // Windows 10 and 11 (no compatibility shims), and themed common controls for the
        // message boxes.
        res_entry(&mut data, RT_MANIFEST, 1, 0x0030, MANIFEST.as_bytes());
        std::fs::write(&res, data).expect("write icon .res");
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
    println!("cargo:rerun-if-changed=src/tray/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");
}

const RT_MANIFEST: u16 = 24;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="Offbeat.MarqueePlayer" version="1.0.0.0" processorArchitecture="amd64"/>
  <description>Marquee Player</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#;

/// Shown in the exe's Properties > Details (and in About: `about::COPYRIGHT`).
const COPYRIGHT: &str = "Copyright \u{a9} 2026 Offbeat";

// ---- version ---------------------------------------------------------------------------
//
// `YYYY.M.D.count`: the build date (local time) and that day's release count. The count advances only for a release build, one made with
// `MP_RELEASE_BUILD=1` (`cargo xtask release` sets it) in a profile that inherits from
// `release`. `.build-version` beside this file keeps the date and count, `.app-version`
// the last release's full number (`cargo xtask release` checks it against the exe's own);
// both are git-ignored. Any other build (`cargo build`, `cargo test`, clippy,
// `cargo xtask build`) is `YYYY.M.D-dev`, counts nothing and writes nothing, so tests and
// quick builds never use up release numbers and a release is numbered once, by the build
// that is packaged.
//
// When the count advances: this script runs again (and numbers the build) when
// `MP_RELEASE_BUILD` changes, and, for a release build only, when any workspace crate's
// source or manifest or Cargo.lock changes, or the compiler does (cargo reruns build
// scripts for a new rustc). It then fingerprints what goes into the exe (`fingerprint`:
// every source, manifest and the lock file, line endings aside, and the compiler's
// version) and keeps the last release's number when that is unchanged; otherwise it takes
// the next one. So a release whose sources differ from the last one gets a new number,
// running the release again with nothing changed keeps it, and the Windows and Linux
// release builds of the same tree in the same checkout, one after the other in either
// order (each with its own target folder, so each runs this script), get the same number.
// Dev builds do not watch the sources (that would recompile the UI on every edit).
//
// A release build with `MP_RELEASE_VERSION` set (`src/release_number.rs`) takes that number
// instead, whatever the date, the state files and the sources (and writes them as for any
// release): CI makes a release's number once and gives it to every system's build, so builds
// on either side of midnight cannot differ. Unset (or empty), nothing changes. A dev build
// ignores it.

struct Version {
    /// "2026.11.15.3", or "2026.11.15-dev".
    text: String,
    /// The four numbers of the Win32 version resource (a dev build's count is 0).
    parts: [u16; 4],
    release: bool,
}

fn app_version() -> Version {
    println!("cargo:rerun-if-env-changed=MP_RELEASE_BUILD");
    let (y, m, d) = local_date();
    let date = format!("{y}.{m}.{d}");
    let asked = std::env::var("MP_RELEASE_BUILD").is_ok_and(|v| v == "1");
    let release_profile = std::env::var("PROFILE").as_deref() == Ok("release");
    if asked && !release_profile {
        println!("cargo:warning=MP_RELEASE_BUILD=1 ignored: not a release-profile build; numbered as a dev build");
    }
    if !(asked && release_profile) {
        return Version { text: format!("{date}-dev"), parts: [y, m, d, 0], release: false };
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    // Every source that goes into the exe: a change to any of them is a new release.
    let workspace = Path::new(&dir).join("..").join("..");
    println!("cargo:rerun-if-changed={}", workspace.join("Cargo.lock").display());
    println!("cargo:rerun-if-changed={}", workspace.join("Cargo.toml").display());
    if let Ok(crates) = std::fs::read_dir(workspace.join("crates")) {
        for c in crates.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()) {
            println!("cargo:rerun-if-changed={}", c.join("Cargo.toml").display());
            println!("cargo:rerun-if-changed={}", c.join("src").display());
        }
    }
    // The UI markup: crates/ui compiles it, so its changes no longer rerun this script on
    // their own.
    println!("cargo:rerun-if-changed={}", Path::new(&dir).join("ui").display());
    let state = Path::new(&dir).join(".build-version");
    let saved = std::fs::read_to_string(&state).ok();
    let print = fingerprint(&workspace);
    println!("cargo:rerun-if-env-changed={}", release_number::VAR);
    let given = std::env::var(release_number::VAR).ok().filter(|v| !v.trim().is_empty());
    let (day, count) = match (given, same_sources(saved.as_deref(), &print)) {
        (Some(v), _) => {
            let [y, m, d, n] = release_number::parse(v.trim()).unwrap_or_else(|e| panic!("{e}"));
            println!("cargo:warning=release {y}.{m}.{d}.{n}: the number {} gives", release_number::VAR);
            (format!("{y}.{m}.{d}"), n)
        }
        (None, Some(kept)) => {
            println!("cargo:warning=release {}.{}: the same sources as the last release, so the same number", kept.0, kept.1);
            kept
        }
        (None, None) => (date.clone(), next_count(saved.as_deref(), &date)),
    };
    let parts: Vec<u16> = day.split('.').filter_map(|p| p.parse().ok()).collect();
    let [y, m, d] = parts[..] else { panic!(".build-version: bad date {day}") };
    let text = format!("{day}.{count}");
    std::fs::write(&state, format!("{day} {count} {print}")).expect("write .build-version");
    std::fs::write(Path::new(&dir).join(".app-version"), &text).expect("write .app-version");
    Version { text, parts: [y, m, d, count], release: true }
}

/// The last release's date and count, when `.build-version` ("YYYY.M.D count fingerprint")
/// says it was made from the sources fingerprinted `print`.
fn same_sources(state: Option<&str>, print: &str) -> Option<(String, u16)> {
    let mut it = state?.split_whitespace();
    let (day, count, saved) = (it.next()?, it.next()?.parse::<u16>().ok()?, it.next()?);
    (saved == print && day.split('.').count() == 3).then(|| (day.to_owned(), count))
}

/// What goes into the exe, as one number: every file under each crate's `src`, its
/// manifest and build script, the UI markup, the patched dependencies, the workspace
/// manifest and lock file (by path relative to the workspace and content, carriage
/// returns dropped so a checkout's line endings do not count), and the compiler's version.
/// FNV-1a, 64 bits, in hex.
fn fingerprint(workspace: &Path) -> String {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = vec![workspace.join("Cargo.toml"), workspace.join("Cargo.lock")];
    if let Ok(crates) = std::fs::read_dir(workspace.join("crates")) {
        for c in crates.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()) {
            for f in ["Cargo.toml", "build.rs"] {
                if c.join(f).is_file() {
                    files.push(c.join(f));
                }
            }
            walk(&c.join("src"), &mut files);
        }
    }
    walk(&workspace.join("crates").join("app").join("ui"), &mut files);
    walk(&workspace.join("crates").join("patches"), &mut files);
    let mut named: Vec<(String, std::path::PathBuf)> =
        files.into_iter().filter_map(|f| Some((f.strip_prefix(workspace).ok()?.to_string_lossy().replace('\\', "/"), f))).collect();
    named.sort();
    named.dedup_by(|a, b| a.0 == b.0);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for (name, path) in &named {
        eat(name.as_bytes());
        eat(&[0]);
        let body: Vec<u8> = std::fs::read(path).unwrap_or_default().into_iter().filter(|&b| b != b'\r').collect();
        eat(&body);
        eat(&[0]);
    }
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    if let Ok(out) = std::process::Command::new(rustc).arg("-V").output() {
        eat(String::from_utf8_lossy(&out.stdout).trim().as_bytes());
    }
    format!("{h:016x}")
}

/// The release count for `date`, from `.build-version` ("YYYY.M.D count"): one more than
/// the last on the same day, else 1.
fn next_count(state: Option<&str>, date: &str) -> u16 {
    let mut it = state.unwrap_or("").split_whitespace();
    match (it.next(), it.next().and_then(|n| n.parse::<u16>().ok())) {
        (Some(d), Some(n)) if d == date => n.saturating_add(1),
        _ => 1,
    }
}

/// Today in local time (the UTC date is the wrong day for part of every day away from UTC).
#[cfg(windows)]
fn local_date() -> (u16, u16, u16) {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }
    let mut t = SystemTime { year: 0, month: 0, day_of_week: 0, day: 0, hour: 0, minute: 0, second: 0, milliseconds: 0 };
    // SAFETY: GetLocalTime fills the SYSTEMTIME it is given and cannot fail.
    unsafe { GetLocalTime(&mut t) };
    (t.year, t.month, t.day)
}

/// Today in local time elsewhere (`date`, which follows TZ and /etc/localtime), else UTC.
#[cfg(not(windows))]
fn local_date() -> (u16, u16, u16) {
    if let Ok(out) = std::process::Command::new("date").arg("+%Y %m %d").output()
        && let [y, m, d] = String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|v| v.parse::<u16>().ok()).collect::<Vec<_>>()[..]
    {
        return (y, m, d);
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    // Days to a civil date (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u16;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u16;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as u16;
    (y, m, d)
}

const RT_VERSION: u16 = 16;

/// The VS_VERSIONINFO resource: the numbers (VS_FIXEDFILEINFO) and the strings Explorer
/// shows in Properties > Details (US English, Unicode).
fn version_info(v: &Version) -> Vec<u8> {
    let [a, b, c, d] = v.parts.map(u32::from);
    let mut fixed = Vec::new();
    for x in [
        0xFEEF_04BD,       // signature
        0x0001_0000,       // structure version
        (a << 16) | b,     // file version, high
        (c << 16) | d,     // file version, low
        (a << 16) | b,     // product version, high
        (c << 16) | d,     // product version, low
        0x3F,              // flags mask
        if v.release { 0 } else { 0x2 }, // VS_FF_PRERELEASE for a dev build
        0x0004_0004,       // VOS_NT_WINDOWS32
        0x1,               // VFT_APP
        0,                 // subtype
        0,                 // date, high
        0,                 // date, low
    ] {
        fixed.extend_from_slice(&u32::to_le_bytes(x));
    }
    let strings = [
        ("CompanyName", "Offbeat"),
        ("FileDescription", "Marquee Player"),
        ("FileVersion", v.text.as_str()),
        ("InternalName", "MarqueePlayer"),
        ("LegalCopyright", COPYRIGHT),
        ("OriginalFilename", "MarqueePlayer.exe"),
        ("ProductName", "Marquee Player"),
        ("ProductVersion", v.text.as_str()),
    ];
    let table: Vec<Vec<u8>> = strings.iter().map(|(k, s)| ver_block(k, &utf16z(s), true, &[])).collect();
    let string_info = ver_block("StringFileInfo", &[], true, &[ver_block("040904B0", &[], true, &table)]);
    // Language en-US (0x0409), code page 1200 (Unicode).
    let translation = [0x0409u16, 0x04B0].iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let var_info = ver_block("VarFileInfo", &[], true, &[ver_block("Translation", &translation, false, &[])]);
    ver_block("VS_VERSION_INFO", &fixed, false, &[string_info, var_info])
}

/// One block of a version resource: its length, its value's length (in characters for
/// text), its type, its key, then the value and the child blocks, each on a 4-byte
/// boundary.
fn ver_block(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0u8; 6];
    b.extend_from_slice(&utf16z(key));
    pad4(&mut b);
    b.extend_from_slice(value);
    for child in children {
        pad4(&mut b);
        b.extend_from_slice(child);
    }
    let value_len = if text { value.len() / 2 } else { value.len() };
    let len = b.len() as u16;
    b[0..2].copy_from_slice(&len.to_le_bytes());
    b[2..4].copy_from_slice(&(value_len as u16).to_le_bytes());
    b[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    b
}

/// UTF-16LE with the terminating zero.
fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

fn pad4(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

// ---- icon ------------------------------------------------------------------------------

/// A compiled resource file (.res) holding icon group 1 with one 32-bit image per size.
fn icon_res(sizes: &[u32]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let mut res = Vec::new();
    // A .res file starts with an empty entry.
    res_entry(&mut res, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // reserved
    group.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    group.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    for (i, &size) in sizes.iter().enumerate() {
        let id = i as u16 + 1;
        let dib = icon_dib(size);
        res_entry(&mut res, RT_ICON, id, 0x1010, &dib);
        let dim = if size >= 256 { 0 } else { size as u8 };
        group.extend_from_slice(&[dim, dim, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes()); // planes
        group.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        group.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    res_entry(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    res
}

/// One resource entry with numeric type and name, language en-US.
fn res_entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    let lang: u16 = if kind == 0 { 0 } else { 0x0409 };
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes()); // header size
    out.extend_from_slice(&[0xff, 0xff]);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0xff, 0xff]);
    out.extend_from_slice(&name.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&lang.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// An icon image in DIB form: BITMAPINFOHEADER (double height), BGRA rows bottom-up,
/// then an all-zero AND mask (the alpha channel does the masking).
fn icon_dib(size: u32) -> Vec<u8> {
    let rgba = icon_art::rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut d = Vec::new();
    for v in [40u32, size, size * 2] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&32u16.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    d.extend_from_slice(&(size * size * 4 + mask_row * size).to_le_bytes());
    d.extend_from_slice(&[0u8; 16]); // resolution, colours
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            d.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    d.resize(d.len() + (mask_row * size) as usize, 0);
    d
}
