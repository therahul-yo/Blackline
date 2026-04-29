use std::io::{self, Write};
use std::thread;
use std::time::Duration;
use colored::Colorize;

use clap::Parser;
use serde::Serialize;

mod stats;
use crate::stats::{
    get_battery_stats, get_cpu_stats, get_network_stats, get_storage_stats, get_top_processes,
    read_meminfo, CpuStats, MemStats,
};

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

// ============= LINUX IMPLEMENTATIONS =============

// ============= MACOS IMPLEMENTATIONS =============

// ============= CROSS-PLATFORM FUNCTIONS =============

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
    let sign = if delta > 0 {
        "+"
    } else if delta < 0 {
        "-"
    } else {
        ""
    };
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
