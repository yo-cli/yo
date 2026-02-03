use crate::ob::config::{ipv6::Ipv6Config, oceanbase::OceanBaseConfig, sysctl::SysctlConfig};
use crate::ob::config::{ConfigFile, ConfigItem, ConfigStatus};
use crate::ob::utils;
use anyhow::Result;
use colored::Colorize;
use std::process::Command;

pub fn run() -> Result<()> {
    println!("[Configuration File Check]\n");

    let mut oceanbase_config = OceanBaseConfig::new();
    let mut sysctl_config = SysctlConfig::new();
    let mut ipv6_config = Ipv6Config::new();

    oceanbase_config.check()?;
    sysctl_config.check()?;
    ipv6_config.check()?;

    let oceanbase_items = oceanbase_config.expected_configs();
    let sysctl_items = sysctl_config.expected_configs();
    let ipv6_items = ipv6_config.expected_configs();

    let mut total_issues = 0;

    // Check OceanBase limits config
    let oceanbase_ok = check_config_items(&oceanbase_items, oceanbase_config.path());
    if !oceanbase_ok {
        total_issues += 1;
    }

    // Check sysctl.conf
    let sysctl_ok = check_sysctl_items(&sysctl_items, sysctl_config.path());
    if !sysctl_ok {
        total_issues += 1;
    }

    // Check IPv6 config
    let ipv6_ok = check_sysctl_items(&ipv6_items, ipv6_config.path());
    if !ipv6_ok {
        total_issues += 1;
    }

    // Runtime check
    println!("\n[Runtime Check]\n");

    let runtime_ok = check_sysctl_runtime()?;
    if !runtime_ok {
        total_issues += 1;
    }

    // Current session check
    println!("\n[Current Session Check]\n");

    let session_ok = check_current_session()?;
    if !session_ok {
        total_issues += 1;
    }

    // Summary
    println!();
    if total_issues == 0 {
        println!("{} All configurations are correct!", "✓".green());
    } else {
        println!(
            "{} Found {} issue(s). Please run 'yo-ob prepare' to reconfigure.",
            "✗".red(),
            total_issues
        );

        if !session_ok {
            println!(
                "\n{} Current session limits need update.",
                "⚠".yellow()
            );
            println!("Please re-login or run: exec su - $USER");
        }
    }

    Ok(())
}

fn check_config_items(items: &[ConfigItem], path: &str) -> bool {
    let ok_count = items
        .iter()
        .filter(|i| i.status == ConfigStatus::Exists)
        .count();
    let total = items.len();

    if ok_count == total {
        println!("  {} {} ({}/{})", "✓".green(), path, ok_count, total);
    } else {
        println!("  {} {}", "✗".red(), path);
        for item in items {
            match item.status {
                ConfigStatus::Exists => {
                    println!("    {} {} {}", "✓".green(), item.key, item.value);
                }
                ConfigStatus::Missing => {
                    println!(
                        "    {} {} {}  [Missing]",
                        "✗".red(),
                        item.key,
                        item.value
                    );
                }
                ConfigStatus::Conflict => {
                    println!(
                        "    {} {} {}  [Current: {}]",
                        "✗".red(),
                        item.key,
                        item.value,
                        item.current_value.as_deref().unwrap_or("unknown")
                    );
                }
            }
        }
    }

    ok_count == total
}

fn check_sysctl_items(items: &[ConfigItem], path: &str) -> bool {
    let ok_count = items
        .iter()
        .filter(|i| i.status == ConfigStatus::Exists)
        .count();
    let total = items.len();

    if ok_count == total {
        println!("  {} {} ({}/{})", "✓".green(), path, ok_count, total);
    } else {
        println!("  {} {}", "✗".red(), path);
        for item in items {
            match item.status {
                ConfigStatus::Exists => {
                    println!("    {} {} = {}", "✓".green(), item.key, item.value);
                }
                ConfigStatus::Missing => {
                    println!(
                        "    {} {} = {}  [Missing]",
                        "✗".red(),
                        item.key,
                        item.value
                    );
                }
                ConfigStatus::Conflict => {
                    println!(
                        "    {} {} = {}  [Current: {}]",
                        "✗".red(),
                        item.key,
                        item.value,
                        item.current_value.as_deref().unwrap_or("unknown")
                    );
                }
            }
        }
    }

    ok_count == total
}

fn check_sysctl_runtime() -> Result<bool> {
    let settings = vec![
        ("fs.aio-max-nr", "1048576"),
        ("vm.max_map_count", "655360"),
        ("net.ipv6.conf.all.disable_ipv6", "1"),
        ("net.ipv6.conf.default.disable_ipv6", "1"),
        ("net.ipv6.conf.lo.disable_ipv6", "1"),
    ];

    let mut all_ok = true;

    for (key, expected) in settings {
        if utils::verify_sysctl(key, expected)? {
            println!("  {} {} = {}", "✓".green(), key, expected);
        } else {
            all_ok = false;
            match Command::new("sysctl").arg(key).output() {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let current = stdout
                        .trim()
                        .strip_prefix(&format!("{} = ", key))
                        .unwrap_or("unknown");
                    println!(
                        "  {} {} = {}  [Expected: {}]",
                        "✗".red(),
                        key,
                        current,
                        expected
                    );
                }
                _ => {
                    println!("  {} {} = ?  [Expected: {}]", "✗".red(), key, expected);
                }
            }
        }
    }

    Ok(all_ok)
}

fn check_current_session() -> Result<bool> {
    let checks = vec![
        ("ulimit -n", "nofile", "655360"),
        ("ulimit -c", "core", "unlimited"),
        ("ulimit -s", "stack", "unlimited"),
        ("ulimit -u", "nproc", "655360"),
    ];

    let mut all_ok = true;

    for (cmd, name, expected) in checks {
        match Command::new("bash").arg("-c").arg(cmd).output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let current = stdout.trim();

                let is_ok = if expected == "unlimited" {
                    current == "unlimited"
                } else {
                    match (current.parse::<u64>(), expected.parse::<u64>()) {
                        (Ok(curr), Ok(exp)) => curr >= exp,
                        _ => current == expected,
                    }
                };

                if is_ok {
                    println!("  {} {} ({}): {}", "✓".green(), name, cmd, current);
                } else {
                    all_ok = false;
                    println!(
                        "  {} {} ({}): {} [Expected: {}]",
                        "✗".red(),
                        name,
                        cmd,
                        current,
                        expected
                    );
                }
            }
            _ => {
                all_ok = false;
                println!("  {} {} ({}): Failed to check", "✗".red(), name, cmd);
            }
        }
    }

    Ok(all_ok)
}
