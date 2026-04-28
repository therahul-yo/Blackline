use std::fs::File;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::thread;
use std::process::Command;
use std::time::Duration;
use colored::Colorize;

use clap::Parser;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(author, version, about = "System resource monitor with aesthetic bars", long_about = None)]
struct Args {
    /// Suppress normal output (useful for scripts)
    #[arg(short, long, help = "Silence normal output; only errors are printed")]
    quiet: bool,

    /// Show values in human‑readable units (MiB/GiB)
    #[arg(short = 'H', long, help = "Display values in MiB/GiB units")]
    human: bool,

    /// Continuously report every <seconds> (0 = run once)
    #[arg(short, long, default_value_t = 1, help = "Polling interval in seconds (default 1) – set 0 for a single snapshot")]
    interval: u64,

    /// Output JSON instead of human‑readable text
    #[arg(long, help = "Print output as formatted JSON")]
    json: bool,

    /// Disable colored output
    #[arg(long, help = "Suppress colored output")]
    no_color: bool,

    /// Show storage information (total/available)
    #[arg(long, help = "Display storage usage (total/available)", default_value_t = true)]
    storage: bool,

    /// Show battery information (percentage, status)
    #[arg(long, help = "Display battery charge and status", default_value_t = true)]
    battery: bool,

    /// Show network traffic (received/transmitted bytes)
    #[arg(long, help = "Display network traffic stats", default_value_t = true)]
    network: bool,

    /// Show CPU usage percentage
    #[arg(short = 'c', long, help = "Display CPU usage", default_value_t = true)]
    cpu: bool,

    /// Show all extended info (storage, battery, network, cpu)
    #[arg(long, help = "Display all extended information")]
    all: bool,

    /// Show top N memory-consuming processes
    #[arg(short = 't', long, help = "Show top N memory-consuming processes", default_value_t = 0)]
    top: u8,

    /// Show delta (change) between intervals instead of absolute values
    #[arg(long, help = "Show change in values between intervals")]
    delta: bool,

    /// Output style: "bars" (default), "compact", or "minimal"
    #[arg(long, default_value = "bars", help = "Output style: bars, compact, or minimal")]
    style: String,

    /// Number of bars for visual display
    #[arg(long, default_value_t = 20, help = "Number of bars for visual display (default 20)")]
    bars: u8,

    /// Show timestamp
    #[arg(long, help = "Show timestamp for each update")]
    timestamp: bool,
}

#[derive(Debug, Serialize, Clone)]
struct MemStats {
    total_kb: u64,
    available_kb: u64,
    used_kb: u64,
    used_percent: f64,
}

#[derive(Debug, Serialize)]
struct CpuStats {
    usage_percent: f32,
    cores: Vec<f32>,
}

#[derive(Default)]
struct CpuRaw {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

// ============= LINUX IMPLEMENTATIONS =============

#[cfg(target_os = "linux")]
fn read_meminfo() -> io::Result<MemStats> {
    read_meminfo_path(Path::new("/proc/meminfo"))
}

#[cfg(target_os = "linux")]
fn read_meminfo_path(path: &Path) -> io::Result<MemStats> {
    let file = File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut total = 0u64;
    let mut available = 0u64;
    for line in reader.lines() {
        let line = line?;
        if line.starts_with("MemTotal:") {
            total = line.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            available = line.split_whitespace().nth(1).unwrap_or("0").parse().unwrap_or(0);
        }
        if total != 0 && available != 0 {
            break;
        }
    }
    let used = total.saturating_sub(available);
    let used_percent = if total > 0 {
        (used as f64 / total as f64) * 100.0
    } else {
        0.0
    };
    Ok(MemStats { total_kb: total, available_kb: available, used_kb: used, used_percent })
}

#[cfg(target_os = "linux")]
fn get_cpu_stats() -> io::Result<CpuStats> {
    let content = std::fs::read_to_string("/proc/stat")?;
    let first_line = content.lines().next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Empty /proc/stat"))?;
    
    let raw = parse_cpu_line(first_line)
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Failed to parse /proc/stat"))?;
    
    let total: u64 = raw.user + raw.nice + raw.system + raw.idle 
        + raw.iowait + raw.irq + raw.softirq + raw.steal;
    let idle: u64 = raw.idle + raw.iowait;
    
    let usage_percent = if total > 0 {
        ((total - idle) as f64 / total as f64) * 100.0
    } else {
        0.0
    };
    
    // Parse per-core stats
    let mut cores = Vec::new();
    for line in content.lines().skip(1) {
        if line.starts_with("cpu") && line.chars().nth(3).map_or(false, |c| c.is_ascii_digit()) {
            if let Some(raw) = parse_cpu_line(line) {
                let total: u64 = raw.user + raw.nice + raw.system + raw.idle 
                    + raw.iowait + raw.irq + raw.softirq + raw.steal;
                let idle: u64 = raw.idle + raw.iowait;
                if total > 0 {
                    let core_usage = ((total - idle) as f64 / total as f64) * 100.0;
                    cores.push(core_usage as f32);
                }
            }
        }
    }
    
    Ok(CpuStats { usage_percent: usage_percent as f32, cores })
}

#[cfg(target_os = "linux")]
fn get_network_stats() -> io::Result<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/net/dev")?;
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in content.lines().skip(2) {
        if let Some(colon) = line.find(':') {
            let data = &line[colon + 1..];
            let fields: Vec<&str> = data.split_whitespace().collect();
            if fields.len() >= 9 {
                let r: u64 = fields[0].parse().unwrap_or(0);
                let t: u64 = fields[8].parse().unwrap_or(0);
                rx += r;
                tx += t;
            }
        }
    }
    Ok((rx / 1024, tx / 1024))
}

// ============= MACOS IMPLEMENTATIONS =============

#[cfg(target_os = "macos")]
fn read_meminfo() -> io::Result<MemStats> {
    // Get total memory using sysctl
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()?;
    let total_bytes: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "Failed to parse hw.memsize"))?;
    let total_kb = total_bytes / 1024;

    // Get memory stats from vm_stat
    let output = Command::new("vm_stat")
        .output()?;
    let vm_output = String::from_utf8_lossy(&output.stdout);
    
    let mut free: u64 = 0;
    let mut active: u64 = 0;
    let mut inactive: u64 = 0;
    let mut wired: u64 = 0;
    let mut compressed: u64 = 0;
    let mut page_size: u64 = 4096; // default page size
    
    // First line gives page size
    if let Some(line) = vm_output.lines().next() {
        if let Some(n) = line.split("PAGESIZE=").nth(1) {
            if let Some(n) = n.split_whitespace().next() {
                page_size = n.parse().unwrap_or(4096);
            }
        }
    }
    
    for line in vm_output.lines() {
        let line = line.trim();
        if line.starts_with("Pages free:") {
            free = extract_page_count(line, page_size);
        } else if line.starts_with("Pages active:") {
            active = extract_page_count(line, page_size);
        } else if line.starts_with("Pages inactive:") {
            inactive = extract_page_count(line, page_size);
        } else if line.starts_with("Pages wired down:") {
            wired = extract_page_count(line, page_size);
        } else if line.starts_with("Compressor page count:") {
            compressed = extract_page_count(line, page_size);
        }
    }
    
    // macOS "available" = free + inactive (app can reclaim)
    // "used" = active + wired + compressed
    let available_kb = (free + inactive) / 1024;
    let used_kb = (active + wired + compressed) / 1024;
    let used_percent = if total_kb > 0 {
        (used_kb as f64 / total_kb as f64) * 100.0
    } else {
        0.0
    };
    
    Ok(MemStats { 
        total_kb, 
        available_kb, 
        used_kb, 
        used_percent 
    })
}

fn extract_page_count(line: &str, page_size: u64) -> u64 {
    // Extract number from "Pages free:         12345."
    if let Some(num_str) = line.split(':').nth(1) {
        let num: u64 = num_str.trim().trim_end_matches('.').parse().unwrap_or(0);
        num * page_size / 1024 // convert pages to KB
    } else {
        0
    }
}

#[cfg(target_os = "macos")]
fn get_cpu_stats() -> io::Result<CpuStats> {
    // Get number of CPUs
    let output = Command::new("sysctl")
        .args(["-n", "hw.ncpu"])
        .output()?;
    let num_cpus: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(1);
    
    // Get CPU usage from top
    let output = Command::new("top")
        .args(["-l", "1", "-n", "0"])
        .output()?;
    let top_output = String::from_utf8_lossy(&output.stdout);
    
    // Parse CPU usage from "CPU usage: X% user, Y% sys, ..."
    let mut usage_percent = 0.0f32;
    for line in top_output.lines() {
        if line.contains("CPU usage:") || line.contains("CPU usage") {
            // Try to parse percentage
            if let Some(pct) = parse_cpu_line_macos(line) {
                usage_percent = pct;
                break;
            }
        }
    }
    
    // If top didn't work, try sysctl for per-core
    let cores = Vec::new();
    
    Ok(CpuStats { usage_percent, cores })
}

#[cfg(target_os = "macos")]
fn parse_cpu_line_macos(line: &str) -> Option<f32> {
    // "CPU usage: 12.3% user, 4.5% sys, 82.1% idle"
    // or "  12.34%  10.23%   7.89%   0.00%   0.00%"
    if let Some(usage_pos) = line.find("CPU usage:") {
        let part = &line[usage_pos..];
        // Find "idle" percentage and calculate used
        if let Some(idle_pos) = part.find("idle") {
            let before_idle = &part[..idle_pos];
            // Look for last percentage before "idle"
            let parts: Vec<&str> = before_idle.split_whitespace().collect();
            for p in parts.iter().rev() {
                if p.ends_with('%') {
                    if let Ok(pct) = p.trim_end_matches('%').parse::<f32>() {
                        return Some(100.0 - pct);
                    }
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn get_network_stats() -> io::Result<(u64, u64)> {
    // Use netstat to get interface statistics
    let output = Command::new("netstat")
        .args(["-ib"])
        .output()?;
    let content = String::from_utf8_lossy(&output.stdout);
    
    let mut rx_bytes: u64 = 0;
    let mut tx_bytes: u64 = 0;
    let skip_interfaces = ["lo", "utun", "awdl", "bridge"];
    
    for line in content.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 7 {
            let name = parts[0];
            // Skip loopback and virtual interfaces
            if skip_interfaces.iter().any(|s| name.starts_with(s)) {
                continue;
            }
            // Bytes columns: Ibytes (6), Obytes (9)
            if let Ok(rx) = parts[6].parse::<u64>() {
                rx_bytes += rx;
            }
            if parts.len() >= 10 {
                if let Ok(tx) = parts[9].parse::<u64>() {
                    tx_bytes += tx;
                }
            }
        }
    }
    
    Ok((rx_bytes / 1024, tx_bytes / 1024))
}

// ============= CROSS-PLATFORM FUNCTIONS =============

fn parse_cpu_line(line: &str) -> Option<CpuRaw> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    Some(CpuRaw {
        user: parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
        nice: parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
        system: parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0),
        idle: parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0),
        iowait: parts.get(5).and_then(|s| s.parse().ok()).unwrap_or(0),
        irq: parts.get(6).and_then(|s| s.parse().ok()).unwrap_or(0),
        softirq: parts.get(7).and_then(|s| s.parse().ok()).unwrap_or(0),
        steal: parts.get(8).and_then(|s| s.parse().ok()).unwrap_or(0),
    })
}

// Get total and available storage (KB) using `df`
fn get_storage_stats() -> io::Result<(u64, u64)> {
    let output = Command::new("df")
        .args(["-k", "/"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "df command failed"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            let total_kb: u64 = parts[1].parse().unwrap_or(0);
            let avail_kb: u64 = parts[3].parse().unwrap_or(0);
            return Ok((total_kb, avail_kb));
        }
    }
    Err(io::Error::new(io::ErrorKind::Other, "Unable to parse df output"))
}

// Get battery percentage and status
fn get_battery_stats() -> Option<(u64, String)> {
    // Try pmset first (more reliable on macOS)
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("pmset")
            .args(["-g", "batt"])
            .output()
            .ok()?;
        let content = String::from_utf8_lossy(&output.stdout);
        
        // Parse "Now drawing from 'Battery Power'\n -InternalBattery-0    85%; discharging; 4:11 remaining"
        for line in content.lines() {
            if line.contains("InternalBattery") || line.contains("Battery") {
                // Extract percentage
                if let Some(pct_start) = line.find(char::is_numeric) {
                    let rest = &line[pct_start..];
                    if let Some(pct) = rest.split(|c: char| !c.is_numeric()).next() {
                        if let Ok(percentage) = pct.parse::<u64>() {
                            // Determine status
                            let status = if line.contains("discharging") {
                                "Discharging"
                            } else if line.contains("charging") {
                                "Charging"
                            } else if line.contains("charged") || line.contains("fully") {
                                "Fully Charged"
                            } else {
                                "Unknown"
                            };
                            return Some((percentage, status.to_string()));
                        }
                    }
                }
            }
        }
    }
    
    // Linux fallback
    #[cfg(target_os = "linux")]
    {
        let capacity_path = Path::new("/sys/class/power_supply/BAT0/capacity");
        let status_path = Path::new("/sys/class/power_supply/BAT0/status");
        let capacity = std::fs::read_to_string(capacity_path).ok()?;
        let status = std::fs::read_to_string(status_path).ok()?;
        let perc = capacity.trim().parse().ok()?;
        return Some((perc, status.trim().to_string()));
    }
    
    None
}

// Get top N memory-consuming processes
fn get_top_processes(n: u8) -> io::Result<Vec<(String, u64)>> {
    let output = Command::new("ps")
        .args(["aux", "-r"])
        .output()?;
    
    if !output.status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "ps command failed"));
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    
    for line in stdout.lines().skip(1).take(n as usize) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 6 {
            let rss_kb: u64 = parts[5].parse().unwrap_or(0);
            // Command is everything after position 10
            let cmd = if parts.len() > 10 {
                parts[10..].join(" ")
            } else {
                parts.iter().skip(10).map(|s| *s).collect::<Vec<_>>().join(" ")
            };
            processes.push((cmd, rss_kb));
        }
    }
    
    Ok(processes)
}

fn format_human(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else {
        format!("{:.1} MiB", mb)
    }
}

fn format_delta_kb(current: i64, previous: i64) -> String {
    let delta = current - previous;
    let sign = if delta >= 0 { "+" } else { "" };
    format!("{}{}", sign, format_human(delta.unsigned_abs() as u64))
}

fn create_bar(percent: f64, bars: u8, no_color: bool) -> String {
    let filled = ((percent / 100.0) * bars as f64).round() as usize;
    let empty = bars as usize - filled;

    if no_color {
        format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
    } else {
        let bar_str = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
        if percent >= 80.0 {
            bar_str.red().to_string()
        } else if percent >= 60.0 {
            bar_str.yellow().to_string()
        } else {
            bar_str.green().to_string()
        }
    }
}

fn display_compact(
    stats: &MemStats,
    args: &Args,
    cpu: Option<&CpuStats>,
    prev_mem: Option<i64>,
    prev_net: Option<(i64, i64)>,
    storage: Option<(u64, u64)>,
    battery: Option<(u64, String)>,
    net: Option<(u64, u64)>,
) -> String {
    let mut parts = Vec::new();

    // Memory
    let mem_str = if args.delta {
        if let Some(prev) = prev_mem {
            let _delta = stats.used_kb as i64 - prev;
            format!("RAM {}", format_delta_kb(stats.used_kb as i64, prev))
        } else {
            format!("RAM {} / {}", format_human(stats.used_kb), format_human(stats.total_kb))
        }
    } else {
        format!("RAM {} / {}", format_human(stats.used_kb), format_human(stats.total_kb))
    };

    if args.no_color {
        parts.push(format!("{} {}", create_bar(stats.used_percent, args.bars, true), mem_str));
    } else {
        parts.push(format!(
            "{} {}",
            create_bar(stats.used_percent, args.bars, false),
            mem_str
        ));
    }

    // CPU
    if args.all || args.cpu {
        if let Some(cs) = cpu {
            let cpu_str = format!("CPU {:.1}%", cs.usage_percent);
            if !args.no_color {
                parts.push(format!("{}", cpu_str.purple()));
            } else {
                parts.push(cpu_str);
            }
        }
    }

    // Storage
    if args.all || args.storage {
        if let Some((total, avail)) = storage {
            let used = total.saturating_sub(avail);
            let percent = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let storage_str = format!("DISK {} / {}", format_human(used), format_human(total));
            if args.no_color {
                parts.push(format!("{} {}", create_bar(percent, args.bars, true), storage_str));
            } else {
                parts.push(format!(
                    "{} {}",
                    create_bar(percent, args.bars, false),
                    storage_str
                ));
            }
        }
    }

    // Battery
    if args.all || args.battery {
        if let Some((perc, status)) = battery {
            let bat_str = format!("BAT {}% ({})", perc, status);
            if !args.no_color {
                parts.push(format!("{}", bat_str.cyan()));
            } else {
                parts.push(bat_str);
            }
        }
    }

    // Network
    if args.all || args.network {
        if let Some((rx, tx)) = net {
            if args.delta {
                if let Some((prev_rx, prev_tx)) = prev_net {
                    parts.push(format!(
                        "NET ↓{} ↑{}",
                        format_delta_kb(rx as i64, prev_rx as i64),
                        format_delta_kb(tx as i64, prev_tx as i64)
                    ));
                }
            } else {
                parts.push(format!("NET ↓{} ↑{}", format_human(rx), format_human(tx)));
            }
        }
    }

    parts.join("  ")
}

fn display_minimal(stats: &MemStats) -> String {
    format!("{:.1}%", stats.used_percent)
}

fn display_top_processes(processes: &[(String, u64)], no_color: bool) -> String {
    let mut lines = Vec::new();
    lines.push(if no_color {
        "Top processes:".to_string()
    } else {
        " Top processes ".cyan().bold().to_string()
    });

    for (i, (cmd, rss)) in processes.iter().enumerate() {
        let num = format!("{}.", i + 1);
        let mem = format_human(*rss);
        if no_color {
            lines.push(format!(
                "  {} {:20} {}",
                num,
                cmd.chars().take(20).collect::<String>(),
                mem
            ));
        } else {
            lines.push(format!(
                "  {} {:20} {}",
                num.cyan(),
                cmd.chars().take(20).collect::<String>().blue(),
                mem.green()
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_human() {
        assert_eq!(format_human(0), "0.0 MiB");
        assert_eq!(format_human(1024), "1.0 MiB");
        assert_eq!(format_human(1048576), "1.0 GiB");
    }

    #[test]
    fn test_bar_creation() {
        assert_eq!(create_bar(50.0, 10, true), "[#####-----]");
        assert_eq!(create_bar(0.0, 10, true), "[----------]");
        assert_eq!(create_bar(100.0, 10, true), "[##########]");
    }

    #[test]
    fn test_delta_format() {
        assert_eq!(format_delta_kb(1000, 800), "+0.2 MiB");
        assert_eq!(format_delta_kb(800, 1000), "-0.2 MiB");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_macos_parse_cpu() {
        let line = "CPU usage: 12.3% user, 4.5% sys, 82.1% idle, 1.1% wait";
        let result = parse_cpu_line_macos(line);
        assert!(result.is_some());
        assert!((result.unwrap() - 17.9).abs() < 0.1);
    }

    #[test]
    fn test_extract_page_count() {
        let page_size = 4096;
        assert_eq!(extract_page_count("Pages free:         12345.", page_size), 12345 * 4);
    }
}

fn main() {
    let args = Args::parse();

    // Validate interval (0‑86400 seconds)
    if args.interval > 86_400 {
        eprintln!("interval must be between 0 and 86400 seconds");
        std::process::exit(1);
    }

    // Initialize previous values for delta mode
    let mut prev_mem: Option<i64> = None;
    let mut prev_net: Option<(i64, i64)> = None;

    // Helper closure to print unless quiet mode is enabled
    let output = |msg: String| {
        if !args.quiet {
            if args.interval > 0 {
                print!("\r{}", msg);
                io::stdout().flush().ok();
            } else {
                println!("{}", msg);
            }
        }
    };

    if args.interval == 0 {
        // Single snapshot mode
        match read_meminfo() {
            Ok(stats) => {
                if args.json {
                    #[derive(Serialize)]
                    struct Output<'a> {
                        memory: &'a MemStats,
                        cpu: Option<&'a CpuStats>,
                    }
                    let cpu = if args.all || args.cpu {
                        get_cpu_stats().ok()
                    } else {
                        None
                    };
                    let out = Output {
                        memory: &stats,
                        cpu: cpu.as_ref(),
                    };
                    println!("{}", serde_json::to_string_pretty(&out).unwrap());
                } else {
                    let cpu = if args.all || args.cpu {
                        get_cpu_stats().ok()
                    } else {
                        None
                    };
                    let storage = if args.all || args.storage {
                        get_storage_stats().ok()
                    } else {
                        None
                    };
                    let battery = if args.all || args.battery {
                        get_battery_stats()
                    } else {
                        None
                    };
                    let net = if args.all || args.network {
                        get_network_stats().ok()
                    } else {
                        None
                    };

                    if args.style == "minimal" {
                        output(display_minimal(&stats));
                    } else {
                        output(display_compact(
                            &stats,
                            &args,
                            cpu.as_ref(),
                            None,
                            None,
                            storage,
                            battery,
                            net,
                        ));
                    }

                    if args.top > 0 {
                        if let Ok(procs) = get_top_processes(args.top) {
                            println!("\n{}", display_top_processes(&procs, args.no_color));
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to read memory info: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // Continuous mode
    loop {
        let timestamp = if args.timestamp {
            format!("[{}] ", chrono_lite_timestamp())
        } else {
            String::new()
        };

        match read_meminfo() {
            Ok(stats) => {
                let cpu = if args.all || args.cpu {
                    get_cpu_stats().ok()
                } else {
                    None
                };
                let storage = if args.all || args.storage {
                    get_storage_stats().ok()
                } else {
                    None
                };
                let battery = if args.all || args.battery {
                    get_battery_stats()
                } else {
                    None
                };
                let net = if args.all || args.network {
                    get_network_stats().ok()
                } else {
                    None
                };

                if args.json {
                    #[derive(Serialize)]
                    struct Output<'a> {
                        memory: &'a MemStats,
                        cpu: Option<&'a CpuStats>,
                    }
                    let out = Output {
                        memory: &stats,
                        cpu: cpu.as_ref(),
                    };
                    println!("{}", serde_json::to_string_pretty(&out).unwrap());
                } else {
                    let display = if args.style == "minimal" {
                        display_minimal(&stats)
                    } else {
                        display_compact(
                            &stats,
                            &args,
                            cpu.as_ref(),
                            prev_mem,
                            prev_net,
                            storage,
                            battery,
                            net,
                        )
                    };
                    output(format!("{}{}", timestamp, display));
                }

                // Update previous values
                prev_mem = Some(stats.used_kb as i64);
                if let Some((rx, tx)) = net {
                    prev_net = Some((rx as i64, tx as i64));
                }
            }
            Err(e) => eprintln!("Failed to read memory info: {}", e),
        }

        thread::sleep(Duration::from_secs(args.interval));
    }
}

// Simple timestamp without chrono dependency
fn chrono_lite_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let secs = now.as_secs();
    let hours = (secs / 3600) % 24;
    let mins = (secs / 60) % 60;
    let secs = secs % 60;
    format!("{:02}:{:02}:{:02}", hours, mins, secs)
}
