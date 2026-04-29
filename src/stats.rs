use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::process::Command;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct MemStats {
    pub total_kb: u64,
    pub available_kb: u64,
    pub used_kb: u64,
    pub used_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct CpuStats {
    pub usage_percent: f32,
    pub cores: Vec<f32>,
}

#[cfg(target_os = "linux")]
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
pub fn read_meminfo() -> io::Result<MemStats> {
    read_meminfo_path(Path::new("/proc/meminfo"))
}

#[cfg(target_os = "linux")]
pub fn read_meminfo_path(path: &Path) -> io::Result<MemStats> {
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
pub fn get_cpu_stats() -> io::Result<CpuStats> {
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
pub fn get_network_stats() -> io::Result<(u64, u64)> {
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
pub fn read_meminfo() -> io::Result<MemStats> {
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
pub fn get_cpu_stats() -> io::Result<CpuStats> {
    // Get number of CPUs
    let output = Command::new("sysctl")
        .args(["-n", "hw.ncpu"])
        .output()?;
    let _num_cpus: u32 = String::from_utf8_lossy(&output.stdout)
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
pub fn get_network_stats() -> io::Result<(u64, u64)> {
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

#[cfg(target_os = "linux")]
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
pub fn get_storage_stats() -> io::Result<(u64, u64)> {
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
pub fn get_battery_stats() -> Option<(u64, String)> {
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
pub fn get_top_processes(n: u8) -> io::Result<Vec<(String, u64)>> {
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
