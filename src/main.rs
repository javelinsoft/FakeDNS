#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use chrono::Timelike;
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, FontId, Layout, Rect, RichText, Sense, Stroke};
use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const APP_VERSION: &str = "1.0";
const WIN_W: f32 = 1180.0;
const WIN_H: f32 = 700.0;
const TITLE: &str = "FakeDNS - DNS Noise";
const MAX_LOG: usize = 5000;
const HARD_MAX_PER_DAY: u32 = 50_000;
const MIN_MAX_PER_DAY: u32 = 500;
/// No two requests are ever more than this many seconds apart.
const MAX_GAP_SECS: f64 = 480.0;
const DEFAULT_MAX_PER_DAY: u32 = 5_000;

/// TLDs that can be enabled for random domains in the GUI.
const ALL_TLDS: &[&str] = &[
    "com", "net", "org", "info", "io", "xyz", "online", "site", "biz", "co", "me", "app", "dev",
    "tech", "store", "shop", "club", "live", "top", "cloud", "blog", "news", "link", "pro", "name",
    "today", "space", "website", "one", "ai", "tv", "us", "uk", "de", "fr", "nl", "ru", "cn", "jp",
    "br", "in", "eu",
];

fn default_tlds() -> Vec<String> {
    ["com", "net", "org", "info", "io", "xyz", "online", "site"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

// Defaults are embedded in the exe, and written to App/ if the files are missing.
const DEF_HOSTS_TXT: &str = include_str!("../App/Hosts.txt");
const DEF_HOSTS_JSON: &str = include_str!("../App/Hosts.json");
const DEF_DNS: &str = include_str!("../App/DNS-Servers.txt");

const TLDS: &[&str] = &[
    "com", "net", "org", "io", "co", "uk", "de", "fr", "edu", "gov", "info", "dev", "app", "ai",
    "me", "tv", "us", "ca", "au", "ru", "cn", "jp", "in", "br", "it", "es", "nl", "se", "no", "fi",
    "pl", "ch", "at", "be", "xyz", "online", "site", "cloud", "biz", "eu",
];

// ---------------------------------------------------------------- paths

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}
fn data_dir() -> PathBuf {
    base_dir().join("Data")
}
fn app_dir() -> PathBuf {
    base_dir().join("App")
}
fn ensure_defaults() {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::create_dir_all(data_dir());
    for (n, c) in [
        ("Hosts.txt", DEF_HOSTS_TXT),
        ("Hosts.json", DEF_HOSTS_JSON),
        ("DNS-Servers.txt", DEF_DNS),
    ] {
        let p = app_dir().join(n);
        if !p.exists() {
            let _ = std::fs::write(p, c);
        }
    }
}
/// Data/<name> if the user has a copy, otherwise App/<name> (defaults).
fn resolve(name: &str) -> PathBuf {
    let d = data_dir().join(name);
    if d.exists() {
        d
    } else {
        app_dir().join(name)
    }
}
fn read_text(name: &str) -> String {
    std::fs::read_to_string(resolve(name)).unwrap_or_default()
}
fn save_file(name: &str, text: &str) -> Result<(), String> {
    std::fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    std::fs::write(data_dir().join(name), text).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- settings

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum ListSource {
    Txt,
    Json,
    None,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum QueryMode {
    A,
    Aaaa,
    Both,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    list: ListSource,
    use_random: bool,
    random_percent: u8,
    max_per_day: u32,
    run_at_startup: bool,
    use_system_dns: bool,
    query_mode: QueryMode,
    tlds: Vec<String>,
    was_running: bool,
    www_enabled: bool,
    www_percent: u8,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            list: ListSource::Txt,
            use_random: false,
            random_percent: 50,
            max_per_day: DEFAULT_MAX_PER_DAY,
            run_at_startup: false,
            use_system_dns: false,
            query_mode: QueryMode::A,
            tlds: default_tlds(),
            was_running: false,
            www_enabled: true,
            www_percent: 50,
        }
    }
}
fn load_settings() -> Settings {
    let mut s: Settings = std::fs::read_to_string(data_dir().join("Settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    s.max_per_day = s.max_per_day.clamp(MIN_MAX_PER_DAY, HARD_MAX_PER_DAY);
    s.random_percent = s.random_percent.clamp(1, 99);
    s.www_percent = s.www_percent.min(100);
    if s.list == ListSource::None {
        s.use_random = true;
    }
    s
}
fn store_settings(s: &Settings) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = save_file("Settings.json", &t);
    }
}

// ---------------------------------------------------------------- startup (registry)

#[cfg(windows)]
fn set_startup(enable: bool) -> Result<(), String> {
    use winreg::{enums::*, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        .map_err(|e| e.to_string())?;
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value("FakeDNS", &format!("\"{}\" --minimized", exe.display()))
            .map_err(|e| e.to_string())
    } else {
        let _ = key.delete_value("FakeDNS");
        Ok(())
    }
}
#[cfg(not(windows))]
fn set_startup(_enable: bool) -> Result<(), String> {
    Err("Run at startup is only supported on Windows".into())
}

// ---------------------------------------------------------------- system DNS

/// DNS servers currently configured by the network / Windows (connected adapters only).
#[cfg(windows)]
fn detect_system_dns() -> Vec<IpAddr> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-NetIPConfiguration | ForEach-Object { $_.DNSServer.ServerAddresses }",
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    let text = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return vec![],
    };
    dedupe_dns(parse_dns(&text))
}
#[cfg(not(windows))]
fn detect_system_dns() -> Vec<IpAddr> {
    let text = std::fs::read_to_string("/etc/resolv.conf").unwrap_or_default();
    let list: Vec<IpAddr> = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("nameserver"))
        .filter_map(|r| r.trim().parse::<IpAddr>().ok())
        .collect();
    dedupe_dns(list)
}
fn dedupe_dns(list: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut seen = HashSet::new();
    list.into_iter()
        .filter(|ip| match ip {
            // site-local placeholder addresses Windows lists when no IPv6 DNS exists
            IpAddr::V6(v6) => v6.segments()[0] != 0xfec0,
            _ => true,
        })
        .filter(|ip| seen.insert(*ip))
        .collect()
}

// ---------------------------------------------------------------- single instance

#[cfg(windows)]
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\FakeDNS_SingleInstance_Mutex"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        // The handle is intentionally kept open for the whole life of the process.
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let err = GetLastError();
        if h as usize == 0 {
            return false;
        }
        err == ERROR_ALREADY_EXISTS
    }
}
#[cfg(not(windows))]
fn already_running() -> bool {
    false
}

// ---------------------------------------------------------------- window show (works while hidden)

/// While the window is hidden eframe does not process viewport commands,
/// so the window is restored directly through the Win32 API.
#[cfg(windows)]
fn win32_show() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd as usize != 0 {
            // Only "restore" a minimized window. Restoring a maximized window would shrink it,
            // so a hidden (tray) window is just shown again in its previous state.
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            } else {
                ShowWindow(hwnd, SW_SHOW);
            }
            SetForegroundWindow(hwnd);
        }
    }
}
#[cfg(not(windows))]
fn win32_show() {}

fn show_window(ctx: &egui::Context) {
    win32_show();
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

// ---------------------------------------------------------------- parsing

fn valid_hostname(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let ok = labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    });
    ok && !labels.last().unwrap().bytes().all(|b| b.is_ascii_digit())
}

fn extract_host(line: &str) -> Option<String> {
    let mut s = line.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") {
        s = &s[7..];
    } else if lower.starts_with("https://") {
        s = &s[8..];
    }
    let end = s.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(s.len());
    s = &s[..end];
    if let Some((_, after)) = s.rsplit_once('@') {
        s = after;
    }
    if let Some((h, port)) = s.rsplit_once(':') {
        if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            s = h;
        }
    }
    let h = s.trim_end_matches('.').to_ascii_lowercase();
    if valid_hostname(&h) {
        Some(h)
    } else {
        None
    }
}

fn parse_txt(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    text.lines()
        .filter_map(extract_host)
        .filter(|h| seen.insert(h.clone()))
        .collect()
}

fn parse_dns(text: &str) -> Vec<IpAddr> {
    text.lines()
        .filter_map(|l| l.trim().parse::<IpAddr>().ok())
        .collect()
}

#[derive(Deserialize)]
#[serde(untagged)]
enum JsonEntry {
    Simple(String),
    Full {
        host: String,
        #[serde(default)]
        subdomains: Vec<String>,
    },
}

/// A subdomain entry may be relative ("www", "api.v2") or a full hostname
/// ("play.google.com", "csp.withgoogle.com").
fn sub_to_fqdn(host: &str, sub: &str) -> String {
    let s = sub.trim().trim_matches('.').to_ascii_lowercase();
    if s == host || s.ends_with(&format!(".{host}")) {
        return s;
    }
    let last = s.rsplit('.').next().unwrap_or("");
    if s.contains('.') && TLDS.contains(&last) {
        return s;
    }
    format!("{s}.{host}")
}

/// Each group = main host followed by its subdomain FQDNs.
fn parse_json(text: &str) -> Result<Vec<Vec<String>>, String> {
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let entries: Vec<JsonEntry> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut groups = Vec::new();
    for e in entries {
        let (host, subs) = match e {
            JsonEntry::Simple(h) => (h, vec![]),
            JsonEntry::Full { host, subdomains } => (host, subdomains),
        };
        let Some(host) = extract_host(&host) else { continue };
        let mut group = vec![host.clone()];
        for s in subs {
            if s.trim().trim_matches('.').is_empty() {
                continue;
            }
            let fqdn = sub_to_fqdn(&host, &s);
            if valid_hostname(&fqdn) && !group.contains(&fqdn) {
                group.push(fqdn);
            }
        }
        groups.push(group);
    }
    Ok(groups)
}

// ---------------------------------------------------------------- DNS

fn build_query(name: &str, qtype: u16, id: u16) -> Vec<u8> {
    let mut p = Vec::with_capacity(64);
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for l in name.split('.') {
        p.push(l.len() as u8);
        p.extend_from_slice(l.as_bytes());
    }
    p.push(0);
    p.extend_from_slice(&qtype.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p
}

fn send_query(server: IpAddr, name: &str, qtype: u16) -> String {
    let bind = if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = match UdpSocket::bind(bind) {
        Ok(s) => s,
        Err(e) => return format!("bind error: {e}"),
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
    let id: u16 = rand::thread_rng().gen();
    let pkt = build_query(name, qtype, id);
    let target = SocketAddr::new(server, 53);
    if let Err(e) = sock.send_to(&pkt, target) {
        return format!("send error: {e}");
    }
    let mut buf = [0u8; 512];
    match sock.recv_from(&mut buf) {
        Ok((n, from)) => {
            if from.ip() != server {
                "reply from unexpected source".into()
            } else if n < 12 || buf[0..2] != id.to_be_bytes() {
                "mismatched reply".into()
            } else {
                format!("OK (rcode {})", buf[3] & 0x0F)
            }
        }
        Err(_) => "no reply (timeout)".into(),
    }
}

// ---------------------------------------------------------------- shared state / worker

struct LogEntry {
    time: String,
    server: IpAddr,
    name: String,
    qtype: &'static str,
    status: String,
}

struct Shared {
    running: AtomicBool,
    reload: AtomicBool,
    settings: Mutex<Settings>,
    log: Mutex<VecDeque<LogEntry>>,
    sent_today: AtomicU32,
    today_target: AtomicU32,
    total: AtomicU64,
    sys_dns: Mutex<Vec<IpAddr>>,
    hist: Mutex<Hist>,
}

fn now_min() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 60) as i64)
        .unwrap_or(0)
}

/// Requests per minute for the last 60 minutes (index 59 = current minute).
struct Hist {
    minute: i64,
    b: [u32; 60],
}
impl Hist {
    fn new() -> Self {
        Self { minute: now_min(), b: [0; 60] }
    }
    fn advance(&mut self) {
        let now = now_min();
        if now != self.minute {
            let d = (now - self.minute).clamp(0, 60) as usize;
            if d > 0 {
                self.b.rotate_left(d);
                for i in 60 - d..60 {
                    self.b[i] = 0;
                }
            }
            self.minute = now;
        }
    }
    fn record(&mut self) {
        self.advance();
        self.b[59] += 1;
    }
}

struct Data {
    groups: Vec<Vec<String>>,
    random: bool,
    random_percent: u32,
    tlds: Vec<String>,
    sys_dns: bool,
    qmode: QueryMode,
    www: bool,
    www_percent: u32,
    dns: Vec<IpAddr>,
}

/// Random hostname: 20-60 random letters/digits label plus a common TLD.
/// "www." is only added to plain registrable names (example.com, example.co.uk),
/// never to names that already are subdomains.
fn can_www(host: &str) -> bool {
    if host.starts_with("www.") {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    match labels.len() {
        2 => true,
        3 => labels[2].len() == 2 && ["co", "com", "org", "net", "gov", "edu", "ac"].contains(&labels[1]),
        _ => false,
    }
}

fn random_domain(rng: &mut impl Rng, tlds: &[String]) -> String {
    const CH: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let len = rng.gen_range(20..=60);
    let label: String = (0..len).map(|_| CH[rng.gen_range(0..CH.len())] as char).collect();
    let tld = if tlds.is_empty() {
        "com"
    } else {
        tlds[rng.gen_range(0..tlds.len())].as_str()
    };
    format!("{label}.{tld}")
}

fn load_data(sh: &Shared) -> Data {
    let st = sh.settings.lock().unwrap().clone();
    let dns = if st.use_system_dns {
        let d = detect_system_dns();
        *sh.sys_dns.lock().unwrap() = d.clone();
        d
    } else {
        parse_dns(&read_text("DNS-Servers.txt"))
    };
    let tlds: Vec<String> = st
        .tlds
        .iter()
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphabetic()))
        .collect();
    let groups = match st.list {
        ListSource::Json => parse_json(&read_text("Hosts.json")).unwrap_or_default(),
        ListSource::Txt => parse_txt(&read_text("Hosts.txt"))
            .into_iter()
            .map(|h| vec![h])
            .collect(),
        ListSource::None => vec![],
    };
    Data {
        groups,
        random: st.use_random || st.list == ListSource::None,
        random_percent: st.random_percent.clamp(1, 99) as u32,
        tlds,
        sys_dns: st.use_system_dns,
        www: st.www_enabled,
        www_percent: st.www_percent.min(100) as u32,
        qmode: st.query_mode,
        dns,
    }
}

fn pick_target(sh: &Shared) -> u32 {
    let max = sh.settings.lock().unwrap().max_per_day.clamp(MIN_MAX_PER_DAY, HARD_MAX_PER_DAY);
    // Random margin of up to 5% below the maximum; never exceeds the maximum.
    let t = (max as f64 * rand::thread_rng().gen_range(0.95..=1.0)) as u32;
    t.clamp(1, max)
}

fn sleep_chunked(sh: &Shared, ms: u64) {
    let start = Instant::now();
    loop {
        let el = start.elapsed().as_millis() as u64;
        if el >= ms || !sh.running.load(Ordering::Relaxed) || sh.reload.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis((ms - el).min(250)));
    }
}

fn randn(rng: &mut impl Rng) -> f64 {
    let u1: f64 = rng.gen_range(f64::EPSILON..1.0);
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

fn lognormal_mean(rng: &mut impl Rng, mean: f64, sigma: f64) -> f64 {
    let mu = mean.max(0.001).ln() - sigma * sigma / 2.0;
    (mu + sigma * randn(rng)).exp()
}

/// Mean seconds between queries right now. The rate is the same around the clock (no day/night
/// difference); it only catches up gently (or slows down) if the day's budget is behind (or ahead).
fn plan_mean_interval(target: u32, sent: u32) -> f64 {
    let now = chrono::Local::now();
    let secs_left = 86_400.0 - (now.hour() * 3600 + now.minute() * 60 + now.second()) as f64;
    let lambda_plan = target as f64 / 86_400.0;
    let expected_rem = target as f64 * secs_left / 86_400.0;
    let remaining = target.saturating_sub(sent) as f64;
    let catch = if expected_rem > 1.0 {
        (remaining / expected_rem).clamp(0.5, 1.5)
    } else {
        1.5
    };
    (1.0 / (lambda_plan * catch)).clamp(0.5, 300.0)
}

/// Human-like gap between "page visits": clustered quick clicks, mostly
/// heavy-tailed idle/reading times, and occasional long breaks.
fn next_gap(rng: &mut impl Rng, visit_mean: f64) -> f64 {
    let mut g = if visit_mean > 6.0 && rng.gen_bool(0.3) {
        (2.0f64.ln() + 0.6 * randn(rng)).exp()
    } else {
        let m = if visit_mean > 6.0 { (visit_mean - 0.3 * 2.4) / 0.7 } else { visit_mean };
        lognormal_mean(rng, m, 1.1)
    };
    if rng.gen_bool(0.02) {
        g += rng.gen_range(120.0..360.0);
    }
    g.clamp(0.3, MAX_GAP_SECS)
}

/// Browser-like gap between lookups of related hostnames of one page:
/// mostly short, skewed, never uniform.
fn sub_gap_ms(rng: &mut impl Rng) -> u64 {
    let g = (0.5f64.ln() + 0.9 * randn(rng)).exp();
    (g.clamp(0.05, 3.0) * 1000.0) as u64
}

/// Which record types to send for one name. In "both" mode the order is random
/// and occasionally only one type is sent, like real resolvers.
fn pick_types(mode: QueryMode, slow: bool, rng: &mut impl Rng) -> Vec<u16> {
    match mode {
        QueryMode::A => vec![1],
        QueryMode::Aaaa => vec![28],
        QueryMode::Both => {
            if slow || rng.gen_bool(0.1) {
                vec![if rng.gen_bool(0.5) { 1 } else { 28 }]
            } else if rng.gen_bool(0.5) {
                vec![1, 28]
            } else {
                vec![28, 1]
            }
        }
    }
}

fn fire(sh: &Shared, ctx: &egui::Context, server: IpAddr, name: &str, qtype: u16) {
    let status = send_query(server, name, qtype);
    sh.sent_today.fetch_add(1, Ordering::Relaxed);
    sh.total.fetch_add(1, Ordering::Relaxed);
    sh.hist.lock().unwrap().record();
    {
        let mut log = sh.log.lock().unwrap();
        if log.len() >= MAX_LOG {
            log.pop_front();
        }
        log.push_back(LogEntry {
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            server,
            name: name.to_string(),
            qtype: if qtype == 1 { "A" } else { "AAAA" },
            status,
        });
    }
    ctx.request_repaint_after(Duration::from_millis(200));
}

fn worker(sh: Arc<Shared>, ctx: egui::Context) {
    let mut rng = rand::thread_rng();
    let mut day = chrono::Local::now().date_naive();
    let mut data = load_data(&sh);
    let mut target = pick_target(&sh);
    let mut last_host = String::new();
    let mut last_sys = Instant::now();
    sh.today_target.store(target, Ordering::Relaxed);
    sh.sent_today.store(0, Ordering::Relaxed);

    loop {
        if !sh.running.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        }
        if data.sys_dns && last_sys.elapsed() > Duration::from_secs(300) {
            data = load_data(&sh);
            last_sys = Instant::now();
        }
        if sh.reload.swap(false, Ordering::Relaxed) {
            data = load_data(&sh);
            last_sys = Instant::now();
            target = pick_target(&sh);
            sh.today_target.store(target, Ordering::Relaxed);
        }
        let today = chrono::Local::now().date_naive();
        if today != day {
            day = today;
            sh.sent_today.store(0, Ordering::Relaxed);
            target = pick_target(&sh);
            sh.today_target.store(target, Ordering::Relaxed);
        }
        if sh.sent_today.load(Ordering::Relaxed) >= target {
            sleep_chunked(&sh, 5_000);
            continue;
        }
        if data.dns.is_empty() || (!data.random && data.groups.is_empty()) {
            sleep_chunked(&sh, 1000);
            continue;
        }

        // Keep enough budget so the log never goes quiet for more than MAX_GAP_SECS
        // until midnight: when the budget runs low, switch to one slow query per visit.
        let now = chrono::Local::now();
        let secs_left = 86_400.0 - (now.hour() * 3600 + now.minute() * 60 + now.second()) as f64;
        let reserve = (secs_left / 400.0) as u32 + 10;
        let slow = sh.sent_today.load(Ordering::Relaxed) + reserve >= target;

        // With both a list and random domains enabled, each visit picks one at random.
        let pick_random =
            data.random && (data.groups.is_empty() || rng.gen_range(0..100) < data.random_percent);
        let mut group = if pick_random {
            vec![random_domain(&mut rng, &data.tlds)]
        } else {
            let mut g = data.groups.choose(&mut rng).unwrap().clone();
            if data.groups.len() > 1 {
                for _ in 0..5 {
                    if g[0] != last_host {
                        break;
                    }
                    g = data.groups.choose(&mut rng).unwrap().clone();
                }
            }
            last_host = g[0].clone();
            g
        };
        if data.www
            && data.www_percent > 0
            && can_www(&group[0])
            && rng.gen_range(0..100) < data.www_percent
        {
            group[0] = format!("www.{}", group[0]);
        }
        if slow {
            group.truncate(1);
        }
        let server = *data.dns.choose(&mut rng).unwrap();

        let started = Instant::now();
        let mut count = 0u32;
        for (i, name) in group.iter().enumerate() {
            if i > 0 {
                sleep_chunked(&sh, sub_gap_ms(&mut rng));
            }
            if !sh.running.load(Ordering::Relaxed)
                || sh.reload.load(Ordering::Relaxed)
                || sh.sent_today.load(Ordering::Relaxed) >= target
            {
                break;
            }
            for (j, qt) in pick_types(data.qmode, slow, &mut rng).into_iter().enumerate() {
                if j > 0 {
                    std::thread::sleep(Duration::from_millis(rng.gen_range(0..12)));
                }
                if sh.sent_today.load(Ordering::Relaxed) >= target {
                    break;
                }
                fire(&sh, &ctx, server, name, qt);
                count += 1;
            }
        }

        let gap = if slow {
            rng.gen_range(400.0..MAX_GAP_SECS - 10.0)
        } else {
            let m = plan_mean_interval(target, sh.sent_today.load(Ordering::Relaxed));
            next_gap(&mut rng, m * count.max(1) as f64)
        } - started.elapsed().as_secs_f64();
        if gap > 0.0 {
            sleep_chunked(&sh, (gap * 1000.0) as u64);
        }
    }
}

// ---------------------------------------------------------------- GUI

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Dashboard,
    HostsTxt,
    HostsJson,
    Dns,
    Settings,
    Log,
}

#[derive(Default)]
struct Counts {
    txt: usize,
    json_hosts: usize,
    json_total: usize,
    dns: usize,
}

struct App {
    shared: Arc<Shared>,
    settings: Settings,
    tab: Tab,
    txt_buf: String,
    json_buf: String,
    dns_buf: String,
    counts: Counts,
    msg: String,
    start_hidden: bool,
    logo: egui::TextureHandle,
    show_help: bool,
    show_about: bool,
    sized: u8,
    _tray: Option<TrayIcon>,
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Procedural icon: purple-to-blue rounded square with a glowing wireframe globe.
fn icon_rgba(size: u32) -> Vec<u8> {
    let px = 2.0 / size as f32;
    let cov = |d: f32, w: f32| ((w - d) / px + 0.5).clamp(0.0, 1.0);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) * px - 1.0;
            let v = (y as f32 + 0.5) * px - 1.0;

            let (qx, qy) = (u.abs() - 0.57, v.abs() - 0.57);
            let d_box = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - 0.35;
            let a_bg = cov(d_box, 0.0);
            let t = ((u + v) * 0.5 + 0.5).clamp(0.0, 1.0);
            let mut col = mix([124.0, 58.0, 237.0], [14.0, 165.0, 233.0], t);

            let r = (u * u + v * v).sqrt();
            let inside = cov(r - 0.62, 0.0);
            col = mix(col, [8.0, 16.0, 48.0], inside * 0.6);

            let ring = cov((r - 0.62).abs(), 0.045);
            let eq = cov(v.abs(), 0.03) * inside;
            let mer = cov(u.abs(), 0.03) * inside;
            let e = ((u / 0.3).powi(2) + (v / 0.62).powi(2)).sqrt();
            let ell = cov((e - 1.0).abs() * 0.3, 0.03) * inside;
            let lat = cov((v - 0.31).abs(), 0.022).max(cov((v + 0.31).abs(), 0.022)) * inside * 0.8;
            let line = ring.max(eq).max(mer).max(ell).max(lat);
            col = mix(col, [225.0, 248.0, 255.0], line);

            let dd = ((u - 0.5).powi(2) + (v + 0.5).powi(2)).sqrt();
            col = mix(col, [255.0, 190.0, 50.0], cov(dd, 0.2) * 0.35);
            col = mix(col, [255.0, 214.0, 10.0], cov(dd, 0.09));

            out.extend_from_slice(&[col[0] as u8, col[1] as u8, col[2] as u8, (a_bg * 255.0) as u8]);
        }
    }
    out
}

fn make_icon() -> Icon {
    Icon::from_rgba(icon_rgba(64), 64, 64).expect("icon")
}


// ---------------------------------------------------------------- style helpers

const BG: Color32 = Color32::from_rgb(22, 27, 46);
const SIDEBAR: Color32 = Color32::from_rgb(17, 21, 39);
const CARD: Color32 = Color32::from_rgb(33, 40, 67);
const CARD_BORDER: Color32 = Color32::from_rgb(58, 68, 106);
const TEXT: Color32 = Color32::from_rgb(250, 251, 255);
const MUTED: Color32 = Color32::from_rgb(236, 240, 252);
const WHITE: Color32 = Color32::WHITE;
const BLUE: Color32 = Color32::from_rgb(110, 175, 255);
const BLUE_D: Color32 = Color32::from_rgb(37, 99, 235);
const GREEN_D: Color32 = Color32::from_rgb(22, 150, 70);
const RED_D: Color32 = Color32::from_rgb(210, 40, 50);
const PURPLE_D: Color32 = Color32::from_rgb(124, 58, 237);
const CYAN: Color32 = Color32::from_rgb(110, 235, 252);
const PURPLE: Color32 = Color32::from_rgb(190, 160, 255);
const GREEN: Color32 = Color32::from_rgb(90, 232, 140);
const ORANGE: Color32 = Color32::from_rgb(255, 190, 120);
const RED: Color32 = Color32::from_rgb(255, 125, 125);
const PINK: Color32 = Color32::from_rgb(255, 130, 190);

fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0_f32, CARD_BORDER);
    v.window_rounding = egui::Rounding::same(14.0);
    v.extreme_bg_color = Color32::from_rgb(44, 54, 92);
    v.faint_bg_color = CARD;
    v.hyperlink_color = CYAN;
    v.selection.bg_fill = BLUE_D;
    v.widgets.noninteractive.bg_fill = CARD;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, CARD_BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.inactive.bg_fill = Color32::from_rgb(50, 60, 95);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(50, 60, 95);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, WHITE);
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, WHITE);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.open.bg_fill = Color32::from_rgb(48, 60, 100);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(48, 60, 100);
    v.widgets.hovered.bg_fill = Color32::from_rgb(68, 82, 125);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(68, 82, 125);
    v.widgets.active.bg_fill = BLUE_D;
    v.widgets.active.weak_bg_fill = BLUE_D;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
    ] {
        w.rounding = egui::Rounding::same(8.0);
    }
    ctx.set_visuals(v);

    let mut st = (*ctx.style()).clone();
    st.spacing.item_spacing = vec2(10.0, 9.0);
    st.spacing.button_padding = vec2(16.0, 7.0);
    st.spacing.interact_size = vec2(40.0, 30.0);
    let mut sc = egui::style::ScrollStyle::solid();
    sc.bar_width = 14.0;
    sc.handle_min_length = 64.0;
    sc.foreground_color = true; // bright handle on the dark-blue track
    st.spacing.scroll = sc;
    st.text_styles.insert(egui::TextStyle::Body, FontId::proportional(17.0));
    st.text_styles.insert(egui::TextStyle::Button, FontId::proportional(17.0));
    st.text_styles.insert(egui::TextStyle::Small, FontId::proportional(14.0));
    st.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(28.0));
    st.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(15.0));
    ctx.set_style(st);
}

fn lerp_col(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn muted(t: impl Into<String>) -> RichText {
    RichText::new(t.into()).color(MUTED)
}

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, CARD_BORDER))
        .rounding(12.0)
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
    ui.add_space(10.0);
}

fn section(ui: &mut egui::Ui, title: &str, color: Color32) {
    ui.label(RichText::new(title).strong().size(19.0).color(color));
    ui.add_space(2.0);
}

fn page_title(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(5.0, 32.0), Sense::hover());
        ui.painter().rect_filled(r, 2.0, color);
        ui.label(RichText::new(text).size(28.0).strong().color(WHITE));
    });
}

fn colored_button(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(WHITE).strong())
            .fill(fill)
            .rounding(8.0)
            .min_size(vec2(96.0, 34.0)),
    )
}

fn stat_tile(ui: &mut egui::Ui, width: f32, label: &str, value: String, sub: &str, accent: Color32) {
    let r = egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, CARD_BORDER))
        .rounding(12.0)
        .inner_margin(egui::Margin::symmetric(18.0, 12.0))
        .show(ui, |ui| {
            let iw = (width - 38.0).max(40.0);
            ui.set_width(iw);
            ui.set_max_width(iw);
            ui.add(egui::Label::new(RichText::new(label).size(13.5).strong().color(MUTED)).truncate());
            ui.add(egui::Label::new(RichText::new(value).size(32.0).strong().color(accent)).truncate());
            ui.add(egui::Label::new(RichText::new(sub).size(14.0).color(MUTED)).truncate());
        });
    let rect = r.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(rect.min + vec2(0.0, 12.0), vec2(4.0, rect.height() - 24.0)),
        2.0,
        accent,
    );
}

fn nav_item(ui: &mut egui::Ui, selected: bool, label: &str, color: Color32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let bg = if selected {
        Color32::from_rgb(48, 60, 100)
    } else if resp.hovered() {
        Color32::from_rgb(38, 47, 79)
    } else {
        Color32::TRANSPARENT
    };
    let p = ui.painter();
    p.rect_filled(rect, 9.0, bg);
    if selected {
        p.rect_filled(
            Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(3.0, rect.height() - 16.0)),
            2.0,
            color,
        );
    }
    p.circle_filled(pos2(rect.left() + 24.0, rect.center().y), 5.5, color);
    p.text(
        pos2(rect.left() + 42.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(17.0),
        if selected { WHITE } else { Color32::from_rgb(230, 234, 250) },
    );
    resp.clicked()
}

/// Square check box with a check mark (egui's default one looks round with our rounded theme).
fn check_box(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let size = 22.0;
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), FontId::proportional(17.0), WHITE);
    let desired = vec2(size + 10.0 + galley.size().x, size.max(galley.size().y) + 4.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let b = Rect::from_min_size(pos2(rect.left(), rect.center().y - size / 2.0), vec2(size, size));
    let fill = if *value {
        BLUE_D
    } else if resp.hovered() {
        Color32::from_rgb(68, 82, 125)
    } else {
        Color32::from_rgb(50, 60, 95)
    };
    let p = ui.painter();
    p.rect_filled(b, 4.0, fill);
    p.rect_stroke(b, 4.0, Stroke::new(1.5_f32, if *value { BLUE } else { MUTED }));
    if *value {
        let (x, y) = (b.left(), b.top());
        let st = Stroke::new(3.0_f32, WHITE);
        p.line_segment([pos2(x + 5.0, y + 11.5), pos2(x + 9.5, y + 16.0)], st);
        p.line_segment([pos2(x + 9.5, y + 16.0), pos2(x + 17.5, y + 6.0)], st);
    }
    p.galley(
        pos2(rect.left() + size + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        WHITE,
    );
    resp
}

fn menu_frame(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .inner_margin(egui::Margin::same(4.0))
        .show(ui, |ui| {
            ui.set_min_width(220.0);
            add(ui);
        });
}

fn menu_item(ui: &mut egui::Ui, label: &str) -> bool {
    let w = ui.available_width().max(220.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
    let bg = if resp.hovered() { BLUE_D } else { CARD };
    let p = ui.painter();
    p.rect_filled(rect, 6.0, bg);
    p.text(
        pos2(rect.left() + 14.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(17.0),
        WHITE,
    );
    resp.clicked()
}

fn window_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, CARD_BORDER))
        .rounding(14.0)
        .inner_margin(egui::Margin::same(16.0))
}

fn status_chip(ui: &mut egui::Ui, running: bool) {
    let (txt, col) = if running { ("RUNNING", GREEN) } else { ("STOPPED", RED) };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 15.0, lerp_col(CARD, col, 0.18));
    p.circle_filled(pos2(rect.left() + 18.0, rect.center().y), 5.0, col);
    p.text(
        pos2(rect.left() + 32.0, rect.center().y),
        Align2::LEFT_CENTER,
        txt,
        FontId::proportional(15.0),
        col,
    );
}

fn draw_chart(ui: &mut egui::Ui, data: &[u32; 60]) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 8.0, Color32::from_rgb(24, 29, 50));
    let max = (*data.iter().max().unwrap()).max(4) as f32;
    let plot = Rect::from_min_max(rect.min + vec2(10.0, 10.0), rect.max - vec2(10.0, 24.0));
    for i in 0..=3 {
        let y = plot.bottom() - plot.height() * i as f32 / 3.0;
        p.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0_f32, Color32::from_rgb(52, 62, 96)),
        );
    }
    let bw = plot.width() / 60.0;
    for (i, &v) in data.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let h = plot.height() * v as f32 / max;
        let x = plot.left() + bw * i as f32;
        let r = Rect::from_min_max(pos2(x + 1.0, plot.bottom() - h), pos2(x + bw - 1.0, plot.bottom()));
        let col = if i == 59 { GREEN } else { lerp_col(CYAN, PURPLE, v as f32 / max) };
        p.rect_filled(r, 2.0, col);
    }
    let f = FontId::proportional(13.0);
    p.text(pos2(plot.left(), rect.bottom() - 5.0), Align2::LEFT_BOTTOM, "60 min ago", f.clone(), MUTED);
    p.text(pos2(plot.right(), rect.bottom() - 5.0), Align2::RIGHT_BOTTOM, "now", f.clone(), MUTED);
}

fn log_row(ui: &mut egui::Ui, e: &LogEntry, short: bool) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let m = |t: String, c: Color32| RichText::new(t).monospace().color(c);
        let time = if short && e.time.len() >= 19 { e.time[11..].to_string() } else { e.time.clone() };
        ui.label(m(time, MUTED));
        ui.label(m(format!("{:<15}", e.server.to_string()), CYAN));
        ui.label(m(format!("{:<4}", e.qtype), if e.qtype == "A" { BLUE } else { PURPLE }));
        let c = if e.status.starts_with("OK (rcode 0)") {
            GREEN
        } else if e.status.starts_with("OK") {
            ORANGE
        } else {
            RED
        };
        // Status comes before the (possibly very long) name so it is never cut off.
        ui.label(m(format!("{:<20}", e.status), c));
        let name = if short && e.name.chars().count() > 44 {
            format!("{}...", e.name.chars().take(41).collect::<String>())
        } else {
            e.name.clone()
        };
        ui.label(m(name, TEXT));
    });
}

fn help_ui(ui: &mut egui::Ui) {
    let h = |ui: &mut egui::Ui, t: &str, c: Color32| {
        ui.add_space(8.0);
        ui.label(RichText::new(t).strong().size(19.0).color(c));
    };
    let p = |ui: &mut egui::Ui, t: &str| {
        ui.label(RichText::new(t).color(TEXT));
    };

    h(ui, "What FakeDNS does", CYAN);
    p(ui, "FakeDNS sends DNS lookups for websites you never visit, mixed in with your real ones, so your real DNS activity is harder to pick out. It never opens those sites - it only asks DNS servers for their addresses.");

    h(ui, "Quick start", GREEN);
    p(ui, "1. Open 'DNS Servers' and check the list (one IP per line). The default is 8.8.8.8.");
    p(ui, "2. Choose where hostnames come from in Settings: Hosts.txt, Hosts.json, random domains, or a list plus random domains.");
    p(ui, "3. Press Start on the Dashboard. FakeDNS never starts sending by itself when you open it. Closing the window hides it in the system tray and it keeps working.");
    p(ui, "4. Use the tray icon to open the window again (click, or right-click > Open GUI) or to exit.");

    h(ui, "Pages", BLUE);
    p(ui, "Dashboard - status, requests sent today, a per-minute chart and the latest lookups. Start/Stop pauses or resumes sending.");
    p(ui, "Hosts.txt / Hosts.json / DNS Servers - edit the lists and press Save. Counts update as you type.");
    p(ui, "Settings - sources, random-domain TLDs, record types, DNS servers, daily limit and startup.");
    p(ui, "Log - every lookup with time, server, type, name and result. The log lives in memory only and is lost on exit.");

    h(ui, "Hosts.txt format", PURPLE);
    p(ui, "One entry per line: a hostname (example.com) or a URL (https://www.example.com/page). The hostname is extracted from URLs. Lines that are not valid hostnames are ignored.");

    h(ui, "Hosts.json format", PINK);
    p(ui, "A list of hosts, each with optional subdomains:");
    ui.label(RichText::new("[\n  {\"host\": \"google.com\",\n   \"subdomains\": [\"play.google.com\", \"mail\"]},\n  \"example.org\"\n]").monospace().color(CYAN));
    p(ui, "A subdomain can be a short name (mail -> mail.google.com) or a full hostname. In this mode a visit looks up the host and then its subdomains a moment apart.");

    h(ui, "DNS servers", GREEN);
    p(ui, "DNS-Servers.txt: one IP address per line; anything else is ignored. A server listed twice is used twice as often. In Settings you can switch to the system / network default DNS servers instead of the list.");

    h(ui, "Settings explained", ORANGE);
    p(ui, "Random domains: made-up names of 20-60 letters and digits with a TLD you choose. They do not exist, so replies show 'rcode 3' - that is normal.");
    p(ui, "Record types: A (IPv4) is the default. AAAA is IPv6. 'A + AAAA' sends both in random order, like a browser.");
    p(ui, "Maximum requests per day: 500 to 50,000 (default 5,000). The real daily total is randomly up to 5% lower. Timing is random, with the same rate day and night, and there are never more than 8 minutes between requests.");
    p(ui, "Run at Windows startup: starts FakeDNS hidden in the tray when you sign in. It starts sending only if it was running when you last used it (you pressed Start and did not press Stop); otherwise it stays stopped. Do not move the program folder afterwards, or startup will stop working (untick and tick the option again after moving).");
    p(ui, "www prefix: on by default. Each visit has a chance (default 50%) of looking up www.<host> instead of <host>. Names that already are subdomains are left unchanged.");

    h(ui, "Files and portability", CYAN);
    p(ui, "FakeDNS.exe, App (default lists) and Data (your edits and settings) live in one folder. Nothing is written elsewhere, except the Windows startup entry if you enable it.");

    h(ui, "Good to know", RED);
    p(ui, "Lookups go out as ordinary unencrypted DNS (UDP port 53). FakeDNS adds noise to your DNS traffic; it does not encrypt or hide your real lookups from your network or ISP.");
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, start_hidden: bool) -> Self {
        apply_style(&cc.egui_ctx);
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &icon_rgba(64)),
            egui::TextureOptions::LINEAR,
        );
        let mut settings = load_settings();
        // FakeDNS never starts sending by itself when opened by hand. When it is launched at
        // Windows startup (--minimized) it resumes only if it was running when last used.
        let auto_run = start_hidden && settings.was_running;
        if settings.was_running != auto_run {
            settings.was_running = auto_run;
            store_settings(&settings);
        }
        let shared = Arc::new(Shared {
            running: AtomicBool::new(auto_run),
            reload: AtomicBool::new(false),
            settings: Mutex::new(settings.clone()),
            log: Mutex::new(VecDeque::new()),
            sent_today: AtomicU32::new(0),
            today_target: AtomicU32::new(0),
            total: AtomicU64::new(0),
            sys_dns: Mutex::new(Vec::new()),
            hist: Mutex::new(Hist::new()),
        });

        // Tray
        let menu = Menu::new();
        let open = MenuItem::new("Open GUI", true, None);
        let exit = MenuItem::new("Exit", true, None);
        let _ = menu.append(&open);
        let _ = menu.append(&exit);
        let open_id = open.id().clone();
        let exit_id = exit.id().clone();
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("FakeDNS")
            .with_icon(make_icon())
            .build()
            .ok();

        {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while let Ok(ev) = MenuEvent::receiver().recv() {
                    if ev.id == open_id {
                        show_window(&ctx);
                    } else if ev.id == exit_id {
                        std::process::exit(0);
                    }
                }
            });
        }
        {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                while let Ok(ev) = TrayIconEvent::receiver().recv() {
                    match ev {
                        TrayIconEvent::DoubleClick { .. }
                        | TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } => show_window(&ctx),
                        _ => {}
                    }
                }
            });
        }

        // Worker
        {
            let sh = shared.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || worker(sh, ctx));
        }

        let mut app = App {
            shared,
            settings,
            tab: Tab::Dashboard,
            txt_buf: read_text("Hosts.txt"),
            json_buf: read_text("Hosts.json"),
            dns_buf: read_text("DNS-Servers.txt"),
            counts: Counts::default(),
            msg: String::new(),
            start_hidden,
            logo,
            show_help: false,
            show_about: false,
            sized: 0,
            _tray: tray,
        };
        app.recount();
        app
    }

    fn recount(&mut self) {
        self.counts.txt = parse_txt(&self.txt_buf).len();
        match parse_json(&self.json_buf) {
            Ok(g) => {
                self.counts.json_hosts = g.len();
                self.counts.json_total = g.iter().map(|x| x.len()).sum();
            }
            Err(_) => {
                self.counts.json_hosts = 0;
                self.counts.json_total = 0;
            }
        }
        self.counts.dns = parse_dns(&self.dns_buf).len();
    }

    fn apply_settings(&mut self) {
        self.settings.max_per_day = self.settings.max_per_day.clamp(MIN_MAX_PER_DAY, HARD_MAX_PER_DAY);
        store_settings(&self.settings);
        *self.shared.settings.lock().unwrap() = self.settings.clone();
        self.shared.reload.store(true, Ordering::Relaxed);
    }

    fn editor(ui: &mut egui::Ui, buf: &mut String) -> bool {
        let mut changed = false;
        egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .max_height((ui.available_height() - 30.0).max(120.0))
            .show(ui, |ui| {
            changed = ui
                .add(
                    egui::TextEdit::multiline(buf)
                        .desired_width(f32::INFINITY)
                        .desired_rows(16)
                        .code_editor(),
                )
                .changed();
        });
        changed
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.start_hidden {
            self.start_hidden = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        // Center the window once, and force a real resize event right after start so the
        // layout is computed for the true window size (no text hidden until a manual resize).
        if self.sized == 0 {
            if let Some(ms) = ctx.input(|i| i.viewport().monitor_size) {
                if ms.x > 100.0 && ms.y > 100.0 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(
                        ((ms.x - WIN_W) / 2.0).max(0.0),
                        ((ms.y - WIN_H) / 2.0 - 20.0).max(0.0),
                    )));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(WIN_W + 2.0, WIN_H + 2.0)));
                    self.sized = 1;
                    ctx.request_repaint();
                }
            }
        } else if self.sized == 1 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(WIN_W, WIN_H)));
            self.sized = 2;
            ctx.request_repaint();
        }
        let shared = self.shared.clone();
        let mut do_exit = false;
        let mut do_hide = false;

        // ---- menu bar
        egui::TopBottomPanel::top("menubar")
            .frame(egui::Frame::none().fill(SIDEBAR).inner_margin(egui::Margin::symmetric(12.0, 7.0)))
            .show(ctx, |ui| {
                egui::menu::bar(ui, |ui| {
                    ui.menu_button(RichText::new("File").size(17.0).strong().color(WHITE), |ui| {
                        let mut close = false;
                        menu_frame(ui, |ui| {
                            if menu_item(ui, "Hide to tray") {
                                do_hide = true;
                                close = true;
                            }
                            if menu_item(ui, "Exit") {
                                do_exit = true;
                                close = true;
                            }
                        });
                        if close {
                            ui.close_menu();
                        }
                    });
                    ui.menu_button(RichText::new("Help").size(17.0).strong().color(WHITE), |ui| {
                        let mut close = false;
                        menu_frame(ui, |ui| {
                            if menu_item(ui, "Usage instructions") {
                                self.show_help = true;
                                close = true;
                            }
                            if menu_item(ui, "About FakeDNS") {
                                self.show_about = true;
                                close = true;
                            }
                        });
                        if close {
                            ui.close_menu();
                        }
                    });
                });
            });

        // ---- sidebar
        egui::SidePanel::left("nav")
            .exact_width(200.0)
            .resizable(false)
            .frame(egui::Frame::none().fill(SIDEBAR).inner_margin(egui::Margin::same(14.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.image(egui::load::SizedTexture::new(self.logo.id(), vec2(40.0, 40.0)));
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.label(RichText::new("FakeDNS").size(24.0).strong().color(WHITE));
                        ui.label(RichText::new("DNS privacy noise").size(13.0).color(MUTED));
                    });
                });
                ui.add_space(18.0);
                let items = [
                    (Tab::Dashboard, "Dashboard", BLUE),
                    (Tab::HostsTxt, "Hosts.txt", CYAN),
                    (Tab::HostsJson, "Hosts.json", PURPLE),
                    (Tab::Dns, "DNS Servers", GREEN),
                    (Tab::Settings, "Settings", ORANGE),
                    (Tab::Log, "Log", PINK),
                ];
                for (t, l, c) in items {
                    if nav_item(ui, self.tab == t, l, c) {
                        self.tab = t;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("v{}", APP_VERSION)).size(13.0).color(MUTED));
                    status_chip(ui, shared.running.load(Ordering::Relaxed));
                });
            });

        // ---- pages
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin::same(20.0)))
            .show(ctx, |ui| match self.tab {
                Tab::Dashboard => {
                    egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).auto_shrink([false, false]).show(ui, |ui| {
                        let running = shared.running.load(Ordering::Relaxed);
                        let sent = shared.sent_today.load(Ordering::Relaxed);
                        let tgt = shared.today_target.load(Ordering::Relaxed);
                        ui.horizontal(|ui| {
                            page_title(ui, "Dashboard", BLUE);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (txt, col) = if running { ("Stop", RED_D) } else { ("Start", GREEN_D) };
                                if colored_button(ui, txt, col).clicked() {
                                    shared.running.store(!running, Ordering::Relaxed);
                                    self.settings.was_running = !running;
                                    store_settings(&self.settings);
                                }
                            });
                        });
                        ui.add_space(8.0);

                        let (host_val, host_sub) = match self.settings.list {
                            ListSource::Txt => (self.counts.txt.to_string(), "in Hosts.txt".to_string()),
                            ListSource::Json => (
                                self.counts.json_hosts.to_string(),
                                format!("{} names with subdomains", self.counts.json_total),
                            ),
                            ListSource::None => ("random".to_string(), "generated names".to_string()),
                        };
                        let host_sub = if self.settings.use_random && self.settings.list != ListSource::None {
                            format!("{} + random", host_sub)
                        } else {
                            host_sub
                        };
                        let (dns_n, dns_sub) = if self.settings.use_system_dns {
                            (shared.sys_dns.lock().unwrap().len(), "system default")
                        } else {
                            (self.counts.dns, "from list")
                        };
                        let w = (ui.available_width() - 36.0) / 4.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            stat_tile(ui, w, "SENT TODAY", sent.to_string(), &format!("of {} planned", tgt), BLUE);
                            stat_tile(ui, w, "SINCE LAUNCH", shared.total.load(Ordering::Relaxed).to_string(), "requests", PURPLE);
                            stat_tile(ui, w, "HOSTS", host_val, &host_sub, CYAN);
                            stat_tile(ui, w, "DNS SERVERS", dns_n.to_string(), dns_sub, GREEN);
                        });
                        ui.add_space(10.0);

                        card(ui, |ui| {
                            ui.horizontal(|ui| {
                                section(ui, "Daily budget", BLUE);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(muted(format!(
                                        "{} / {}   (limit {})",
                                        sent, tgt, self.settings.max_per_day
                                    )));
                                });
                            });
                            let frac = if tgt > 0 { sent as f32 / tgt as f32 } else { 0.0 };
                            ui.add(
                                egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                    .fill(BLUE)
                                    .desired_height(12.0)
                                    .rounding(6.0),
                            );
                        });

                        card(ui, |ui| {
                            let b = {
                                let mut h = shared.hist.lock().unwrap();
                                h.advance();
                                h.b
                            };
                            let peak = *b.iter().max().unwrap();
                            ui.horizontal(|ui| {
                                section(ui, "Requests per minute - last hour", CYAN);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(format!("peak {peak}/min")).color(MUTED));
                                });
                            });
                            draw_chart(ui, &b);
                        });

                        card(ui, |ui| {
                            section(ui, "Recent lookups", PINK);
                            let log = shared.log.lock().unwrap();
                            if log.is_empty() {
                                ui.label(muted("No lookups yet."));
                            }
                            for e in log.iter().rev().take(4) {
                                log_row(ui, e, true);
                            }
                        });
                    });
                }
                Tab::HostsTxt => {
                    page_title(ui, "Hosts.txt", CYAN);
                    ui.add_space(4.0);
                    ui.label(muted("One host or URL per line. Invalid lines are ignored."));
                    ui.horizontal(|ui| {
                        if colored_button(ui, "Save", BLUE_D).clicked() {
                            self.msg = match save_file("Hosts.txt", &self.txt_buf) {
                                Ok(_) => {
                                    shared.reload.store(true, Ordering::Relaxed);
                                    "Hosts.txt saved".into()
                                }
                                Err(e) => format!("Save failed: {e}"),
                            };
                        }
                        ui.label(RichText::new(format!("{} valid hosts", self.counts.txt)).color(CYAN));
                        ui.label(muted(self.msg.clone()));
                    });
                    card(ui, |ui| {
                        if Self::editor(ui, &mut self.txt_buf) {
                            self.recount();
                        }
                    });
                }
                Tab::HostsJson => {
                    page_title(ui, "Hosts.json", PURPLE);
                    ui.add_space(4.0);
                    ui.label(muted(r#"Format: [{"host": "example.com", "subdomains": ["www", "play.example.com"]}, "plain.com"]"#));
                    ui.horizontal(|ui| {
                        if colored_button(ui, "Save", BLUE_D).clicked() {
                            self.msg = match parse_json(&self.json_buf) {
                                Err(e) => format!("Invalid JSON: {e}"),
                                Ok(_) => match save_file("Hosts.json", &self.json_buf) {
                                    Ok(_) => {
                                        shared.reload.store(true, Ordering::Relaxed);
                                        "Hosts.json saved".into()
                                    }
                                    Err(e) => format!("Save failed: {e}"),
                                },
                            };
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} hosts, {} names total",
                                self.counts.json_hosts, self.counts.json_total
                            ))
                            .color(PURPLE),
                        );
                        ui.label(muted(self.msg.clone()));
                    });
                    card(ui, |ui| {
                        if Self::editor(ui, &mut self.json_buf) {
                            self.recount();
                        }
                    });
                }
                Tab::Dns => {
                    page_title(ui, "DNS Servers", GREEN);
                    ui.add_space(4.0);
                    ui.label(muted("One DNS server IP per line. Anything else is ignored."));
                    if self.settings.use_system_dns {
                        ui.colored_label(ORANGE, "System default DNS is enabled in Settings; this list is not used.");
                    }
                    ui.horizontal(|ui| {
                        if colored_button(ui, "Save", BLUE_D).clicked() {
                            self.msg = match save_file("DNS-Servers.txt", &self.dns_buf) {
                                Ok(_) => {
                                    shared.reload.store(true, Ordering::Relaxed);
                                    "DNS-Servers.txt saved".into()
                                }
                                Err(e) => format!("Save failed: {e}"),
                            };
                        }
                        ui.label(RichText::new(format!("{} valid servers", self.counts.dns)).color(GREEN));
                        ui.label(muted(self.msg.clone()));
                    });
                    card(ui, |ui| {
                        if Self::editor(ui, &mut self.dns_buf) {
                            self.recount();
                        }
                    });
                }
                Tab::Settings => {
                    egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).auto_shrink([false, false]).show(ui, |ui| {
                        page_title(ui, "Settings", ORANGE);
                        ui.add_space(8.0);
                        let mut changed = false;

                        card(ui, |ui| {
                            section(ui, "Host source", CYAN);
                            changed |= ui
                                .radio_value(&mut self.settings.list, ListSource::Txt, "Hosts.txt (simple list)")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.settings.list, ListSource::Json, "Hosts.json (with subdomains)")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.settings.list, ListSource::None, "No list (random domains only)")
                                .changed();
                            if self.settings.list == ListSource::None {
                                self.settings.use_random = true;
                            }
                            ui.scope(|ui| {
                                changed |= check_box(ui, 
                                        &mut self.settings.use_random,
                                        "Also use random domains (20-60 random letters/digits)",
                                    )
                                    .changed();
                            });
                            if self.settings.list != ListSource::None && self.settings.use_random {
                                ui.horizontal(|ui| {
                                    ui.label("Share of random domains:");
                                    changed |= ui
                                        .add(egui::Slider::new(&mut self.settings.random_percent, 1..=99).suffix("%"))
                                        .changed();
                                });
                            }
                        });

                        card(ui, |ui| {
                            section(ui, "Random domain TLDs", PURPLE);
                            ui.label(muted("Click to enable or disable (at least one stays on). Used when random domains are active."));
                            ui.scope(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    for t in ALL_TLDS {
                                        let on = self.settings.tlds.iter().any(|x| x.as_str() == *t);
                                        let b = egui::Button::new(
                                            RichText::new(format!(".{t}")).color(if on { WHITE } else { MUTED }),
                                        )
                                        .fill(if on { PURPLE_D } else { Color32::from_rgb(48, 58, 92) })
                                        .rounding(14.0);
                                        if ui.add(b).clicked() {
                                            if on {
                                                if self.settings.tlds.len() > 1 {
                                                    self.settings.tlds.retain(|x| x.as_str() != *t);
                                                    changed = true;
                                                }
                                            } else {
                                                self.settings.tlds.push(t.to_string());
                                                changed = true;
                                            }
                                        }
                                    }
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Select all").clicked() {
                                        self.settings.tlds = ALL_TLDS.iter().map(|s| s.to_string()).collect();
                                        changed = true;
                                    }
                                    if ui.button("Reset to default").clicked() {
                                        self.settings.tlds = default_tlds();
                                        changed = true;
                                    }
                                });
                            });
                        });

                        card(ui, |ui| {
                            section(ui, "www prefix", CYAN);
                            changed |= check_box(ui, 
                                    &mut self.settings.www_enabled,
                                    "Randomly add www. in front of host names",
                                )
                                .changed();
                            ui.horizontal(|ui| {
                                ui.label("Chance of adding www:");
                                changed |= ui
                                    .add(egui::Slider::new(&mut self.settings.www_percent, 0..=100).suffix("%"))
                                    .changed();
                            });
                            ui.label(muted("Decided once per visit, for the main host name. Names that are already subdomains, and the subdomains in Hosts.json, are left alone."));
                        });

                        card(ui, |ui| {
                            section(ui, "DNS record types", BLUE);
                            changed |= ui
                                .radio_value(&mut self.settings.query_mode, QueryMode::A, "A only (IPv4) - default")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.settings.query_mode, QueryMode::Aaaa, "AAAA only (IPv6)")
                                .changed();
                            changed |= ui
                                .radio_value(&mut self.settings.query_mode, QueryMode::Both, "A + AAAA (like a browser)")
                                .changed();
                        });

                        card(ui, |ui| {
                            section(ui, "DNS servers to query", GREEN);
                            changed |= ui
                                .radio_value(
                                    &mut self.settings.use_system_dns,
                                    false,
                                    "List from DNS-Servers.txt (default)",
                                )
                                .changed();
                            changed |= ui
                                .radio_value(
                                    &mut self.settings.use_system_dns,
                                    true,
                                    "System / network default DNS",
                                )
                                .changed();
                            if self.settings.use_system_dns {
                                let list = shared.sys_dns.lock().unwrap();
                                if list.is_empty() {
                                    ui.label(muted("Detecting system DNS servers..."));
                                } else {
                                    let txt: Vec<String> = list.iter().map(|i| i.to_string()).collect();
                                    ui.label(RichText::new(format!("Detected: {}", txt.join(", "))).color(CYAN));
                                }
                            }
                        });

                        card(ui, |ui| {
                            section(ui, "Limits and startup", ORANGE);
                            ui.horizontal(|ui| {
                                ui.label("Maximum DNS requests per day:");
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut self.settings.max_per_day)
                                            .range(MIN_MAX_PER_DAY..=HARD_MAX_PER_DAY),
                                    )
                                    .changed();
                            });
                            ui.label(muted(format!(
                                "Allowed {MIN_MAX_PER_DAY}-{HARD_MAX_PER_DAY}. The daily total is randomly up to 5% below the maximum; never more than 8 minutes between requests."
                            )));
                        });
                        if changed {
                            self.apply_settings();
                        }

                        card(ui, |ui| {
                            if check_box(ui, &mut self.settings.run_at_startup, "Run at Windows startup")
                                .changed()
                            {
                                self.msg = match set_startup(self.settings.run_at_startup) {
                                    Ok(_) => String::new(),
                                    Err(e) => {
                                        self.settings.run_at_startup = false;
                                        e
                                    }
                                };
                                self.apply_settings();
                            }
                            ui.label(muted("Do not move this program folder if startup is enabled."));
                            if !self.msg.is_empty() {
                                ui.colored_label(RED, &self.msg);
                            }
                        });
                    });
                }
                Tab::Log => {
                    ui.horizontal(|ui| {
                        page_title(ui, "Log", PINK);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if colored_button(ui, "Clear", Color32::from_rgb(70, 82, 124)).clicked() {
                                shared.log.lock().unwrap().clear();
                            }
                        });
                    });
                    ui.label(muted("In-memory log of the last 5000 requests (never written to disk)."));
                    ui.add_space(6.0);
                    card(ui, |ui| {
                        let log = shared.log.lock().unwrap();
                        let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y + 4.0;
                        egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                            .stick_to_bottom(true)
                            .max_height((ui.available_height() - 20.0).max(150.0))
                            .auto_shrink([false, false])
                            .show_rows(ui, row_h, log.len(), |ui, range| {
                                for i in range {
                                    if let Some(e) = log.get(i) {
                                        log_row(ui, e, false);
                                    }
                                }
                            });
                    });
                }
            });

        // ---- windows
        let mut open = self.show_help;
        egui::Window::new("How to use FakeDNS")
            .open(&mut open)
            .frame(window_frame())
            .default_size([600.0, 560.0])
            .collapsible(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).show(ui, |ui| help_ui(ui));
            });
        self.show_help = open;

        let mut open = self.show_about;
        egui::Window::new("About FakeDNS")
            .open(&mut open)
            .frame(window_frame())
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.image(egui::load::SizedTexture::new(self.logo.id(), vec2(72.0, 72.0)));
                    ui.label(RichText::new("FakeDNS").size(26.0).strong().color(WHITE));
                    ui.label(muted(format!("Version {}", APP_VERSION)));
                    ui.add_space(6.0);
                    ui.label("Portable DNS privacy noise generator.");
                });
            });
        self.show_about = open;

        if do_hide {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if do_exit {
            self._tray = None; // remove the tray icon cleanly before quitting
            std::process::exit(0);
        }
    }
}

fn main() -> eframe::Result<()> {
    if already_running() {
        // Another copy is running: just bring its window to the front.
        win32_show();
        return Ok(());
    }
    ensure_defaults();
    let minimized = std::env::args().any(|a| a == "--minimized");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([WIN_W, WIN_H])
            .with_min_inner_size([WIN_W, WIN_H])
            .with_icon(std::sync::Arc::new(egui::IconData {
                rgba: icon_rgba(64),
                width: 64,
                height: 64,
            })),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, minimized)))),
    )
}
